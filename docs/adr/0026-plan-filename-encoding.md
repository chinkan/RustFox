# ADR-0026: Percent-encoded plan filenames + Windows ADS migration

- **Status:** Accepted
- **Date:** 2026-10-14
- **Target release:** v1.0.6
- **Depends on:** none

## Context

Plans were saved as `.plans/{title}.json` since v1.0.4. On Windows NTFS a title
containing `:` (AI titles such as `Plan: A`) is parsed as `file:stream`, so
`Plan: A.json` writes a 0-byte base file `Plan` plus a hidden alternate data
stream ` A.json`. Directory listing never shows the stream, so the plan is
invisible. Titles with two or more colons (`a:b:c`) failed to save at all.

## Decision

1. **Filename format.** `p_` + percent-encoding of the title's UTF-8 bytes.
   Only ASCII `[a-z0-9_-]` is kept; every other byte (uppercase letters, `:`,
   `/`, `\`, `.`, space, non-ASCII bytes) becomes `%XX` with uppercase hex.
   Encoding uppercase letters keeps `Plan` (`p_%50lan.json`) and `plan`
   (`p_plan.json`) distinct on case-insensitive filesystems (NTFS, APFS).
   The prefix keeps reserved device names (`CON`, `NUL`, `COM1`, ...) from
   ever being a stem.
2. **Long titles.** If the name would exceed 120 chars, the encoded body is
   truncated (never inside a `%XX` triple) and the file is named
   `h_<truncated>_<16 hex of SHA-256(title)>.json`. The distinct `h_` prefix
   means no short title can ever encode to a hashed name.
3. **Title source of truth** is the JSON `title` field. Listing reads every
   `*.json` in the plans dir and uses its JSON title; filenames are never
   decoded and `*.tmp` files are ignored.
4. **Conflict rule.** When a lookup finds a file at the encoded name, its JSON
   `title` must equal the requested title byte-for-byte. Otherwise create,
   update and view fail with
   `Another plan named "<existing title>" already exists. Please choose a different name.`
   and the file is never overwritten.
5. **Backward compatibility (no rename).** Lookup tries the encoded name
   first, then any `*.json` whose JSON title matches, so old `{title}.json`
   files on Linux/macOS keep working. Updates and re-creates write back to
   the file where the plan was found; old files are not renamed (no
   delete/rename on the hot path, no duplicate listing).
6. **Windows-only ADS migration**, once per plans dir per process, on first
   plan access. Errors are logged with `tracing::warn!` and never propagate.
   - Leftover `*.tmp` files in the plans dir are deleted first.
   - Every regular file (any extension, e.g. base `Plan`) has its named
     streams enumerated with `FindFirstStreamW`/`FindNextStreamW` (minimal
     `kernel32` FFI, no new dependency; `ERROR_HANDLE_EOF` ends the scan,
     other errors are logged). The unnamed `::$DATA` stream and streams not
     ending in `.json` are skipped.
   - Stream `: A.json:$DATA` on base `Plan` gives title `Plan: A` (leading
     space preserved); the JSON `title` wins when present.
   - The stream must parse as JSON. It is written to `<target>.tmp` in the
     same dir, moved to the target with `MoveFileExW` **without**
     `MOVEFILE_REPLACE_EXISTING` (`MOVEFILE_WRITE_THROUGH` only), re-read
     and verified, and only then is the stream deleted.
   - **Collision:** if the target exists (pre-check or
     `ERROR_FILE_EXISTS`/`ERROR_ALREADY_EXISTS`), the existing file is never
     touched. The plan is recovered as `<title> (recovered)`, then
     `<title> (recovered 2)`, `(recovered 3)`, ... with its JSON `title`
     updated. Only if that fails is the stream kept, with a warning.
   - The base file is deleted only if a stream was migrated from it, no named
     streams remain and it is 0 bytes; ordinary extensionless files are never
     touched. A second run changes nothing.

## Consequences

- Plans with `:` are visible on Windows; `a:b:c` and case-only-different
  titles save everywhere.
- New plans use new on-disk names; old names stay readable indefinitely.
- The `cfg(windows)` test uses a real NTFS ADS fixture (two streams on one
  base, a pre-placed target, a leftover `.tmp`, plain extensionless files)
  and skips itself when the filesystem has no ADS (e.g. Wine on Unix).
