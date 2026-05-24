# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.1] - 2026-05-24

### Changed

- Bump `windows-sys` from 0.59.0 to 0.61.2

## [0.2.0] - 2026-05-17

### Added

- `--port-file` option to write the mapped public port to a file.
- `GPL-3.0-only` license.

## [0.1.0] - 2026-05-16

### Added

- Initial release: NAT-PMP client that maps a port through the gateway and
  pushes the assigned public port to qBittorrent, with a renewal loop. Built
  for ProtonVPN's port-forwarding gateway. Linux, macOS, and Windows targets.

[Unreleased]: https://github.com/Martin8412/natpmp-rs/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/Martin8412/natpmp-rs/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/Martin8412/natpmp-rs/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Martin8412/natpmp-rs/releases/tag/v0.1.0
