# Architecture

NomNom is a local Linux application with two deliberately separate interfaces:

- a Ratatui terminal library and book-details view
- a GTK4 reading window

The EPUB layer owns archive validation, package metadata, spine order, chapter extraction, and cover decoding. Reader state uses stable chapter and paragraph coordinates rather than visual page numbers, because reflow changes pagination.

SQLite state is stored below the XDG state directory. Configuration is stored below the XDG config directory. EPUB files are never modified, renamed, or moved automatically.

## Boundaries

- `epub`: document parsing and normalized reading content
- `library`: filesystem navigation, cover preview, and details
- `progress`: saved location and per-book reading preferences
- `reader`: native focused-reading presentation and input
- `speech`: asynchronous Kokoro synthesis and PipeWire playback

## Performance rules

- Do not unpack an EPUB to disk.
- Keep archive and image work off the GTK event path where practical.
- Persist content-relative locations, never viewport-dependent page numbers.
- Treat malformed chapters as local failures where possible rather than rejecting an otherwise readable book.
- Keep model loading and synthesis off the GTK event thread; retain the Kokoro process between passages.

## Speech

Press `s` to speak the selected text, or the active passage when there is no selection. Press `x` to stop. Navigating also stops playback so speech never drifts away from the visible passage.

Kokoro runs locally through the Python package in the Nix development shell. The default configuration uses American English (`a`), the `af_heart` voice, 24 kHz output, and normal speed. It can be adjusted with `NOMNOM_TTS_LANGUAGE`, `NOMNOM_TTS_VOICE`, `NOMNOM_TTS_SPEED`, and `NOMNOM_TTS_PYTHON`. The model is fetched by Kokoro on first use and is not stored in this repository.

Dictionary lookup uses the local WordNet database included in the Nix environment, with `dict` and `sdcv` still supported when installed.
