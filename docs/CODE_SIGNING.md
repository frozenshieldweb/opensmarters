# Code signing policy

> **Status:** opensmarters is applying to [SignPath Foundation](https://signpath.org)'s free
> code signing program for open-source projects. Until the application is approved, the
> Windows releases are **not signed** and Windows SmartScreen may warn about an unknown
> publisher. This page describes how signing works once it is in place.
>
> When approved: *Free code signing provided by [SignPath.io](https://signpath.io),
> certificate by [SignPath Foundation](https://signpath.org).*

## What is signed

| File | Signed |
|---|---|
| `opensmarters-<version>-windows-x64-setup.exe` (the installer; it also becomes `uninstall.exe`) | Yes |
| `opensmarters.exe` (the app, inside the installer and the portable zip) | Yes |
| `libmpv-2.dll` (the video engine: an unmodified upstream build of mpv and FFmpeg, see [THIRD_PARTY.md](../packaging/THIRD_PARTY.md)) | No: it isn't built from this repository |

Only files built from this repository's source are signed. Signed files carry the product
name "opensmarters" and the release version in their file properties.

## How releases are built

- A release starts when a maintainer pushes a version tag (`vX.Y.Z`) that matches the
  version in `Cargo.toml`.
- [`.github/workflows/release.yml`](../.github/workflows/release.yml) builds it on a clean
  GitHub-hosted Windows runner from that tag, with [`tools/package.ps1`](../tools/package.ps1).
  Nothing is built on a personal computer.
- Inputs are pinned: the Rust toolchain (`rust-toolchain.toml`), every crate
  (`Cargo.lock`, built with `--locked`), every GitHub Action (by commit SHA), and the
  libmpv download (hash-checked by `tools/fetch-libmpv.ps1`).
- Each release file gets a signed [build provenance attestation](https://docs.github.com/actions/security-for-github-actions/using-artifact-attestations)
  and is listed with its SHA-256 in `SHA256SUMS.txt`. You can check a download with
  `gh attestation verify <file> --repo frozenshieldweb/opensmarters`.
- Build scripts and workflow files are part of the source and go through the same review
  as the code.

## Team and roles

| Role | Who | What they do |
|---|---|---|
| Committers and reviewers | [frozenshieldweb](https://github.com/frozenshieldweb) (repository owner) | Change the source. Changes from anyone else arrive as pull requests and are reviewed by a committer before they are merged. |
| Approvers | [frozenshieldweb](https://github.com/frozenshieldweb) | Approve each signing request by hand, after checking that it comes from the release workflow for a tagged version. |

Everyone with one of these roles uses multi-factor authentication on GitHub and on
SignPath.

## Privacy

opensmarters sends nothing to any server except the ones you choose:

- **Your IPTV provider**: the server, playlist and guide addresses you enter.
- **GitHub**, only when you check for updates in Settings › About (or turn on the weekly
  check, which is off by default). It asks for the latest version number. GitHub sees your
  IP address; nothing else is sent, and nothing is downloaded or installed.

There is no telemetry, no analytics and no account. The installer makes no network
connections at all. Uninstall from Windows Settings › Apps; your data and saved passwords
are only removed if you tick those options. Details: [SECURITY.md](SECURITY.md).

## Reporting a problem

A security problem, or a signed file you think wasn't built this way: see
[SECURITY.md › Reporting a vulnerability](SECURITY.md#9-reporting-a-vulnerability).
