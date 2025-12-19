# Spectral (WIP)

A real-time audio noise suppression application powered by [RNNoise](https://arxiv.org/pdf/1709.08243), a hybrid DSP and neural-network–based noise suppression system originally developed by Xiph.org.

Development status: currently functional on Linux via pipewire. It has no persistent state yet, so the application must be running to use. 

## Development

Enter the development environment:
```bash
nix develop
```

Build the project:
```bash
nix build
```

Run the project:
```bash
nix run
```

Or use standard Cargo commands:
```bash
cargo build
cargo run
cargo test
```
