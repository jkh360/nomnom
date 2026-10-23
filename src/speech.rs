use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};

const KOKORO_HELPER: &str = r#"
import logging
import os
import sys
import tempfile
import traceback
import warnings

os.environ.setdefault('HF_HUB_DISABLE_PROGRESS_BARS', '1')
warnings.filterwarnings('ignore', category=FutureWarning)
warnings.filterwarnings('ignore', message='dropout option adds dropout.*')
logging.getLogger('huggingface_hub').setLevel(logging.ERROR)

import numpy as np
import soundfile as sf
from kokoro import KPipeline

pipeline = KPipeline(
    lang_code=os.environ.get('NOMNOM_TTS_LANGUAGE', 'a'),
    repo_id='hexgrad/Kokoro-82M',
)
voice = os.environ.get('NOMNOM_TTS_VOICE', 'af_heart')
speed = float(os.environ.get('NOMNOM_TTS_SPEED', '1.0'))
print('READY', flush=True)

while True:
    line = sys.stdin.buffer.readline()
    if not line:
        break
    try:
        size = int(line)
        text = sys.stdin.buffer.read(size).decode('utf-8')
        sys.stdin.buffer.read(1)
        chunks = [audio for _, _, audio in pipeline(text, voice=voice, speed=speed)]
        if not chunks:
            raise RuntimeError('Kokoro produced no audio')
        audio = np.concatenate(chunks)
        handle, path = tempfile.mkstemp(prefix='nomnom-', suffix='.wav')
        os.close(handle)
        sf.write(path, audio, 24000)
        print('OK ' + path, flush=True)
    except Exception:
        message = traceback.format_exc().replace('\n', ' | ')
        print('ERR ' + message, flush=True)
"#;

pub struct Speech {
    sender: Sender<CommandMessage>,
    generation: Arc<AtomicU64>,
    player: Arc<Mutex<Option<Child>>>,
}

enum CommandMessage {
    Speak { text: String, generation: u64 },
    Shutdown,
}

struct Kokoro {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Speech {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let player = Arc::new(Mutex::new(None));
        let worker_generation = Arc::clone(&generation);
        let worker_player = Arc::clone(&player);
        thread::spawn(move || {
            let mut kokoro = None;
            while let Ok(message) = receiver.recv() {
                match message {
                    CommandMessage::Speak { text, generation } => {
                        stop_player(&worker_player);
                        if worker_generation.load(Ordering::Acquire) != generation {
                            continue;
                        }
                        let result = synthesize(&mut kokoro, &text).and_then(|path| {
                            if worker_generation.load(Ordering::Acquire) != generation {
                                let _ = fs::remove_file(path);
                                return Ok(());
                            }
                            play(path, generation, &worker_generation, &worker_player)
                        });
                        if let Err(error) = result {
                            eprintln!("nomnom: speech failed: {error:#}");
                            kokoro = None;
                        }
                    }
                    CommandMessage::Shutdown => break,
                }
            }
            stop_player(&worker_player);
            if let Some(mut engine) = kokoro {
                let _ = engine.child.kill();
                let _ = engine.child.wait();
            }
        });
        Self {
            sender,
            generation,
            player,
        }
    }

    pub fn speak(&self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        stop_player(&self.player);
        let _ = self.sender.send(CommandMessage::Speak {
            text: text.to_owned(),
            generation,
        });
    }

    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        stop_player(&self.player);
    }
}

impl Drop for Speech {
    fn drop(&mut self) {
        self.stop();
        let _ = self.sender.send(CommandMessage::Shutdown);
    }
}

fn synthesize(engine: &mut Option<Kokoro>, text: &str) -> Result<String> {
    if engine.is_none() {
        *engine = Some(Kokoro::start()?);
    }
    let engine = engine.as_mut().expect("Kokoro was initialized");
    writeln!(engine.input, "{}", text.len())?;
    engine.input.write_all(text.as_bytes())?;
    engine.input.write_all(b"\n")?;
    engine.input.flush()?;

    let mut response = String::new();
    if engine.output.read_line(&mut response)? == 0 {
        bail!("Kokoro helper exited unexpectedly");
    }
    let response = response.trim_end();
    if let Some(path) = response.strip_prefix("OK ") {
        Ok(path.to_owned())
    } else if let Some(message) = response.strip_prefix("ERR ") {
        bail!("{message}")
    } else {
        bail!("unexpected Kokoro response: {response}")
    }
}

impl Kokoro {
    fn start() -> Result<Self> {
        let python = env::var_os("NOMNOM_TTS_PYTHON").unwrap_or_else(|| "python3".into());
        let mut child = Command::new(python)
            .args(["-u", "-c", KOKORO_HELPER])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("could not start Python/Kokoro; run NomNom from its Nix development shell")?;
        let input = child.stdin.take().context("Kokoro stdin is unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("Kokoro stdout is unavailable")?;
        let mut output = BufReader::new(stdout);
        let mut startup_output = Vec::new();
        loop {
            let mut line = String::new();
            if output.read_line(&mut line)? == 0 {
                let status = child.wait().ok();
                let details = startup_output.join(" | ");
                if details.is_empty() {
                    bail!("Kokoro failed to initialize{status:?}");
                }
                bail!("Kokoro failed to initialize{status:?}: {details}");
            }
            let line = line.trim();
            if line == "READY" {
                break;
            }
            if !line.is_empty() {
                startup_output.push(line.to_owned());
            }
        }
        Ok(Self {
            child,
            input,
            output,
        })
    }
}

fn play(
    path: String,
    generation: u64,
    current_generation: &AtomicU64,
    player: &Mutex<Option<Child>>,
) -> Result<()> {
    let child = Command::new("pw-play")
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("could not start pw-play")?;
    *player.lock().unwrap_or_else(|error| error.into_inner()) = Some(child);
    loop {
        if current_generation.load(Ordering::Acquire) != generation {
            stop_player(player);
            break;
        }
        let finished = {
            let mut guard = player.lock().unwrap_or_else(|error| error.into_inner());
            match guard.as_mut() {
                Some(child) => child.try_wait()?.is_some(),
                None => true,
            }
        };
        if finished {
            player
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            break;
        }
        thread::sleep(Duration::from_millis(30));
    }
    let _ = fs::remove_file(path);
    Ok(())
}

fn stop_player(player: &Mutex<Option<Child>>) {
    if let Some(mut child) = player
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}
