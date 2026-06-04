# Contributing to FamilyClaw

Thank you for your interest in FamilyClaw! This project is a sovereign multi-agent family operating system written in Rust, and we welcome contributions that improve the platform for everyone.

## Code of Conduct

Be respectful. Be constructive. This is a family project — act like you belong to one.

## Quick Start

```bash
git clone https://github.com/Sisuthros/familyclaw.git
cd familyclaw
cargo check     # verify it compiles
cargo test      # run the full suite
cargo clippy    # lint check (warnings = errors)
```

## Architecture

FamilyClaw has two strictly separated layers:

- **Layer A** (this repo, open source) — the platform, generic examples only
- **Layer B** (private, never published) — souls, profiles, API keys

**Nothing from Layer B may ever reach this repository.** See [ARCHITECTURE.md](docs/ARCHITECTURE.md) for details.

## Contribution Guidelines

### What we welcome

- Bug fixes and test coverage improvements
- New crate features that fit the platform (memory, emotion, dreaming, latent, security, etc.)
- Documentation improvements
- Performance optimizations with benchmarks
- CI/CD improvements

### What we don't accept

- Any files from Layer B (soul files, calibration data, API keys, private profiles)
- Feature additions without tests
- Changes that break `cargo clippy -- -D warnings`
- Commits with `unsafe` code (it's forbidden at the workspace level)

### Commit Style

- Use [Conventional Commits](https://www.conventionalcommits.org/): `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`
- Keep commits atomic — one logical change per commit
- Write commit messages in English

### Pull Request Process

1. Fork the repository
2. Create a feature branch from `main`
3. Make your changes
4. Ensure `cargo test` passes
5. Ensure `cargo clippy -- -D warnings` passes
6. Open a PR with a clear description of what and why

### Testing

- Every new feature must include tests
- Run the full suite before submitting: `cargo test --all`
- Doc tests count — use `///` documentation with runnable examples

## Security

If you find a security vulnerability, please report it privately via GitHub Security Advisories rather than opening a public issue.

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).