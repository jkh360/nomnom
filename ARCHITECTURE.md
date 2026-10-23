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

## Performance rules

- Do not unpack an EPUB to disk.
- Keep archive and image work off the GTK event path where practical.
- Persist content-relative locations, never viewport-dependent page numbers.
- Treat malformed chapters as local failures where possible rather than rejecting an otherwise readable book.
