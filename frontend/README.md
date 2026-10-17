# Minutes - Frontend

A modern desktop application for recording, transcribing, and analyzing meetings with AI assistance. Built with Next.js and Tauri for a native desktop experience.

## Features

- Real-time audio recording from both microphone and system audio
- Live transcription using Whisper ASR (locally running)
- Native desktop integration using Tauri
- Speaker diarization support
- Rich text editor for note-taking
- Privacy-focused: All processing happens locally

## Prerequisites

Minutes is Mac-only: Apple Silicon, macOS 13 or later.

- Node.js (v18 or later)
- Rust (latest stable)
- pnpm (v9.15.9)
- [Xcode Command Line Tools](https://developer.apple.com/download/all/?q=xcode)


## Project Structure

```
/frontend
├── src/                   # Next.js frontend code
├── src-tauri/             # Rust backend for Tauri
├── public/                # Static assets
└── package.json           # Project dependencies
```

## Installation

Use `pnpm install --frozen-lockfile` to install the committed dependency set. When intentionally changing dependencies, update and commit `pnpm-lock.yaml`; frozen installation fails when it is out of sync.

1. Install prerequisites:
   ```bash
   # Install Homebrew if not already installed
   /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
   
   # Install Node.js
   brew install node
   
   # Install Rust
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   
   # Install pnpm
   npm install -g pnpm@9.15.9
   
   # Install Xcode Command Line Tools
   xcode-select --install
   ```

2. Clone the repository and navigate to the frontend directory:
   ```bash
   git clone https://github.com/praveenvnktsh/minutes.git
   cd minutes/frontend
   ```
  

3. Install dependencies:
   ```bash
   pnpm install --frozen-lockfile
   ```

## Running the App

Use the provided script to run the app in development mode:
```bash
./clean_run.sh
```

To build a production version:
```bash
./clean_build.sh
```

You can specify the log level (info, debug, trace):
```bash
./clean_run.sh debug
```

You can also use the package scripts directly:
```bash
pnpm run tauri:dev
pnpm run tauri:build
```

## Local Transcription

Current Minutes does not require a separate FastAPI service, Docker backend, or manually started whisper-server process. Local transcription is handled by the Rust/Tauri desktop app.

For build and acceleration details, see:

- [Building from Source](../docs/BUILDING.md)
- [GPU Acceleration](../docs/GPU_ACCELERATION.md)
- [Architecture](../docs/architecture.md)

## Development

### Frontend (Next.js)
- The frontend is built with Next.js and Tailwind CSS
- Source code is in the `src/` directory
- To run only the frontend: `pnpm run dev`

### Backend (Tauri)
- The Rust backend is in the `src-tauri/` directory
- Handles audio capture, file system access, transcription, storage, and native integrations
- To run only the Tauri development server: `pnpm run tauri:dev`

## Troubleshooting

- If you encounter permission issues with scripts, make them executable:
  ```bash
  chmod +x clean_run.sh clean_build.sh
  ```
- For microphone access issues, ensure the app has microphone permissions in System Settings

## Contributing

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add some amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## License

This project is licensed under the MIT License - see the LICENSE file for details.
