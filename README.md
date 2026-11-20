# kv-downloader

```text
██╗  ██╗██╗   ██╗      ██████╗  ██████╗ ██╗    ██╗███╗   ██╗██╗      ██████╗  █████╗ ██████╗ ███████╗██████╗
██║ ██╔╝██║   ██║      ██╔══██╗██╔═══██╗██║    ██║████╗  ██║██║     ██╔═══██╗██╔══██╗██╔══██╗██╔════╝██╔══██╗
█████╔╝ ██║   ██║█████╗██║  ██║██║   ██║██║ █╗ ██║██╔██╗ ██║██║     ██║   ██║███████║██║  ██║█████╗  ██████╔╝
██╔═██╗ ╚██╗ ██╔╝╚════╝██║  ██║██║   ██║██║███╗██║██║╚██╗██║██║     ██║   ██║██╔══██║██║  ██║██╔══╝  ██╔══██╗
██║  ██╗ ╚████╔╝       ██████╔╝╚██████╔╝╚███╔███╔╝██║ ╚████║███████╗╚██████╔╝██║  ██║██████╔╝███████╗██║  ██║
╚═╝  ╚═╝  ╚═══╝        ╚═════╝  ╚═════╝  ╚══╝╚══╝ ╚═╝  ╚═══╝╚══════╝ ╚═════╝ ╚═╝  ╚═╝ ╚══════╝╚═╝  ╚═╝
```

[![CI](https://github.com/Daddidio/kv-downloader/actions/workflows/ci.yml/badge.svg?branch=kvui-c25-http-release)](https://github.com/Daddidio/kv-downloader/actions/workflows/ci.yml)

`kv-downloader` automates downloading the individual tracks from songs purchased through [Karaoke Version](https://www.karaoke-version.com/). It is intended for building your own backing-track mix in software such as Logic Pro.

> [!IMPORTANT]
> This workflow is specific to how I use Karaoke Version. You are welcome to fork the project. Pull requests may be accepted when the changes are useful and general enough.

## kv-downloader or kvui?

This fork focuses on **KVUI**, an interactive terminal application for browsing purchased Karaoke Version songs, selecting individual tracks, and downloading backing-track stems.

The repository contains two applications that share a common Rust core:

| | `kvui` | `kv-downloader` |
|---|---|---|
| Interface | Interactive terminal UI | Command-line interface |
| Development status | Actively developed in this fork | Legacy; no new feature development planned |
| Song selection | Browse and search purchased songs | Supply a song URL |
| Track selection | Interactive selection | Command-line options |
| Download method | HTTP only | HTTP by default; legacy Chromium option remains |
| Progress | Per-track status, retries, and completion summary | Console logging |
| Resume functionality | Retired | Legacy options remain; behavior not currently verified |
| Executable | `kvui` | `kv_downloader` |

### KVUI download behavior

KVUI uses the simplified HTTP downloader. The Chromium-based download option has been retired from KVUI.

Chromium-related browser functionality may still be invoked for authentication, server challenges, or other required interactions with Karaoke Version. This does not mean Chromium is being used to download the audio files.

The HTTP downloader includes safeguards designed to prevent incorrect or stale audio from being published, including request provenance checks, cached-file validation, and safe staging of downloaded files.

When a cached MP3 cannot be verified as belonging to the requested song and mix, KVUI rejects it rather than treating it as a successful download. This conservative behavior may cause some otherwise valid cached downloads to fail after retries.

KVUI does not provide the former saved-progress resume workflow. Failed tracks can be retried through the normal download process.

The existing artist/song directory structure, configured download location, and key-specific file organization remain unchanged.

## Requirements

- Windows, macOS, or Linux
- A Karaoke Version account with purchased songs
- A terminal capable of running the KVUI text interface
- Chromium available for authentication and website security challenges when required

KVUI downloads audio using HTTP. Chromium is not used as an alternative audio download method.

**Platform testing:** The current KVUI development and regression testing has been performed on Windows. Compatibility with macOS and Linux has not been independently verified for this fork.

## Installation

### Download a release

This fork does not currently provide published binary releases.

The verified KVUI development version is maintained in the repository's development branches. Users who wish to run it should build it from source using the instructions below.

Future releases, if published, will be available on the [GitHub Releases page](https://github.com/Daddidio/kv-downloader/releases).

**Note:** A tested local Windows executable does not necessarily mean a downloadable GitHub release has been published.

### Install from source

Install Rust and Cargo using the official [Rust installation instructions](https://www.rust-lang.org/tools/install).

Clone this fork and check out the KVUI development branch:

```bash
git clone https://github.com/Daddidio/kv-downloader.git
cd kv-downloader
git switch kvui-c25-http-release
```

Build KVUI:

```bash
cargo build --release -p kvui
```

The compiled executable will be located in `target/release/` (`kvui.exe` on Windows).

The legacy CLI can also be built separately:

```bash
cargo build --release -p cli
```

**Development note:** Development note: The current KVUI development work has been validated using Windows debug builds. The optimized release build has not yet undergone the same regression testing.

The `kvui-c25-http-release` branch will become publicly available only after the reviewed changes have been approved and pushed to this fork.

## Authentication

You can authenticate from either interface:

```bash
kv_downloader auth
```

or launch `kvui` and select **Login**.

Credentials are stored using your operating system's secure credential store. They are sent only to Karaoke Version. Both applications share the same stored authentication.

For development, a `.env` file may instead provide:

```dotenv
KV_USERNAME=your_username
KV_PASSWORD=your_password
```

Log out with `kv_downloader logout` or the **Logout** action in `kvui`. Logging out of KVUI removes stored authentication information and clears its local song catalog. Downloaded audio files are not deleted by this action.

## kvui terminal interface

Launch the terminal UI with:

```bash
kvui
```

From a source checkout:

```bash
cargo run -p kvui
```

<p align="center">
  <img src="docs/screenshots/kvui-menu.png" alt="kvui main menu" width="800">
</p>

### Browsing and selecting tracks

KVUI maintains a locally cached catalog of purchased Karaoke Version songs. Songs can be browsed and sorted without repeatedly retrieving the complete catalog.

When the song list opens, it defaults to **Title A–Z** sorting.

<p align="center">
  <img src="docs/screenshots/kvui-songs.png" alt="KVUI purchased-song catalog" width="49%">
  <img src="docs/screenshots/kvui-song.png" alt="KVUI track selection screen" width="49%">
</p>

Song-list controls:

- `S` — toggle between **Title A–Z** and **Artist A–Z**.
- `Up/Down` — move through the song list.
- `Enter` — open the selected song.
- `Esc` — return to the previous screen.

Returning from a song's details preserves the selected sorting method and highlighted song.

After opening a song, select the individual tracks to download, configure the intro count, and choose the desired transposition.

Track selection includes controls for selecting or deselecting all tracks. Follow the on-screen instructions for the available keyboard shortcuts.

Choose **Set Download Folder** from the main menu to configure the download destination. KVUI remembers the selected location between sessions.

The existing artist/song folder hierarchy and key-specific organization are preserved.

### Download status and completion

`kvui` displays each track's state, the current retry attempt, live diagnostic logs, and a final summary. Completed tracks remain available even when another track fails.

<p align="center">
  <img src="docs/screenshots/kvui-download.png" alt="kvui live download and retry status" width="49%">
  <img src="docs/screenshots/kvui-complete.png" alt="kvui partial-success completion summary" width="49%">
</p>

## How kvui works

### Song catalog synchronization

- The first browse synchronizes every page of purchased songs in date-descending order.
- Songs are stored in a local SQLite catalog in the platform-specific application-data directory.
- Later visits display cached songs immediately and perform an incremental refresh in the background.
- Incremental synchronization stops when it reaches a song already present in the catalog.
- **Resync Songs List** performs a complete, all-pages refresh.
- If synchronization fails partway through, the existing catalog remains available.
- Song sorting happens locally, independently of Karaoke Version's current server ordering. The song list defaults to Title A–Z, with Artist A–Z available through the sort toggle.

### Downloads, retries, and cache safety

KVUI uses the simplified HTTP downloader. Chromium-based audio downloading is no longer supported in the KVUI interface.

The download workflow is:

1. KVUI retrieves the selected song's available tracks and original key.
2. The user selects the desired tracks, intro count setting, and transposition.
3. Each selected track is requested and downloaded separately through HTTP.
4. Failed downloads are retried according to the downloader's retry rules. A failure in one track does not necessarily prevent other selected tracks from completing.
5. Successfully downloaded files are published to the configured download destination using safeguards designed to prevent partial or incorrect files from replacing valid audio.

#### Cached MP3 verification

Karaoke Version may return previously generated MP3 files when processing a download request.

KVUI uses request provenance checks to help ensure that a cached response corresponds to the requested song, track selection, intro count, and transposition.

- Cached audio is accepted only when the available request metadata and file identity establish verified provenance.
- Server-provided filenames, timestamps, or readiness indicators alone are not considered sufficient proof that a cached MP3 belongs to the current request.
- When provenance cannot be verified, KVUI rejects the staged file and allows the normal retry process to handle the failure.
- Unverified cached audio is not published as a successful download.

**Limitation:** A legitimate cached MP3 may be rejected if its provenance cannot be established. This is intentional: KVUI favors rejecting an uncertain download over silently publishing audio from the wrong mix.

#### Resume functionality

The former saved-progress resume system and its progress-reset controls have been retired from KVUI.

KVUI retains its normal retry behavior for download failures, but does not offer the previous persistent resume workflow.

Existing downloaded audio files and the established artist/song directory organization are preserved.

### Browser authentication and security challenges

KVUI uses direct HTTP requests for audio downloads. Chromium remains available for authentication and interactions required by Karaoke Version's website security systems.

When a browser interaction is required, KVUI may open a Chromium window. This does not indicate that the audio download method has switched from HTTP to Chromium.

The browser integration may be used to establish or refresh an authenticated session and obtain the cookies or other session information needed for HTTP requests.

If a security challenge interrupts a download, KVUI may refresh the browser session and retry the affected request according to its normal retry rules.

**Important limitations:**

- Chromium-based audio downloading is not available as an alternative method in KVUI.
- Repeated authentication or security challenges may cause a download to fail.
- Previously completed downloads are not automatically resumed through the retired saved-progress workflow.
- Server-side changes to authentication, security challenges, or download responses may require future updates to KVUI.

The legacy command-line application retains separate Chromium-related options, but those are not part of KVUI's supported download workflow.

### Diagnostics

The on-screen log follows new messages automatically. Up/Down pauses following, and End resumes it. To save detailed diagnostics to the default `kvui-diagnostics.log` file, run:

```bash
kvui --diagnostics
```

Choose another path with:

```bash
kvui --diagnostics /path/to/kvui.log
```

Diagnostic output is filtered to this application and redacts cookie values.

## Legacy kv-downloader command-line interface

The repository retains the original `kv_downloader` command-line application for compatibility and historical reference.

**Development status:** This fork focuses on KVUI. No new CLI features are currently planned, and the legacy CLI has not undergone the same recent regression testing as KVUI.

### Basic usage

After purchasing a song through Karaoke Version, supply its URL:

```bash
kv_downloader download <song-url>
```

Common options:

- `-d <path>` — set the download location.
- `-t <offset>` — transpose by semitones, within the supported range of -4 to +4.
- `--count-in` — include the intro precount.
- `--tracks` — specify individual tracks by name.
- `--force-restart` — request a fresh download rather than using previous progress information.
- `--debug` — enable detailed logging.

Example:

```bash
kv_downloader download <song-url> -d my_song_dir --count-in -t -1
```

From a source checkout:

```bash
cargo run -p cli -- download <song-url> -d my_song_dir --count-in
```

### Legacy Chromium options

The CLI source still includes:

- `--browser` — select Chromium browser automation instead of HTTP downloading.
- `--headless` — run Chromium without a visible browser window when browser automation is selected.
- `--browser-idle-timeout-secs` — configure the browser idle timeout.

These options belong to the legacy CLI and are **not available as alternative download methods in KVUI**.

The legacy Chromium download path and CLI resume behavior have not been validated as part of the current KVUI regression testing. Their presence in the source code should not be interpreted as a guarantee of continued operation.

For normal use of this fork, **KVUI is the recommended application**.

## Screenshot automation

The documentation screenshots use deterministic demo data and do not access an account. On macOS, with WezTerm and ImageMagick installed, regenerate them with:

```bash
./scripts/capture-tui-screenshots.sh
```

macOS must grant Screen Recording and Accessibility permission to the terminal running the script.

## Use at your own risk

This tool automates a process that you would otherwise perform manually. Avoid huge or concurrent batches; abusive automation may violate Karaoke Version's terms or put your account at risk.

And Karaoke Version, if you're listening: we'd love for this workflow to be officially supported in the UI.

## License

This source code is released under the MIT license.
