# Code Signing Policy — Tumbler

**Last updated:** 1 Sep 2026

> **Status: not yet in effect.** An application to the SignPath Foundation is
> pending. Releases published before that application is approved are
> **unsigned**. This document describes the signing arrangement Tumbler intends
> to adopt; it will be updated to the present tense, and this notice removed,
> when the first signed release ships.

## Overview

Once signing is in effect, Tumbler Windows releases will be digitally signed to
ensure authenticity and integrity. Code signing helps verify that:

1. The software genuinely comes from the Tumbler project
2. The code has not been tampered with since it was signed
3. You can trust the source of the application

## Certificate Information

The expected properties of the signature:

| Property | Value |
|---|---|
| **Publisher** | SignPath Foundation |
| **Signature Algorithm** | SHA256 |
| **Timestamp Server** | SignPath Foundation |
| **Certificate Type** | Code Signing Certificate |

The certificate is held by the SignPath Foundation, so "SignPath Foundation" —
not the project or its maintainer — is the publisher name Windows will show.

## How to Verify

### Windows

1. Right-click the downloaded `.exe` file
2. Select **Properties**
3. Go to the **Digital Signatures** tab
4. Select the signature and click **Details**
5. Verify the signer is "SignPath Foundation"

### Command Line

```powershell
Get-AuthenticodeSignature "Tumbler_x.y.z_x64-setup.exe"
```

## Why SignPath Foundation?

Tumbler is a free and open-source project. SignPath Foundation provides code
signing certificates to qualifying open-source projects at no cost, allowing
the project to reduce Windows SmartScreen warnings, provide verified downloads,
and maintain security in its release process.

## Build Verification

Signed releases will be built through an automated CI/CD pipeline:

- **Build System:** GitHub Actions (`.github/workflows/release.yml`, triggered by a `v*` tag)
- **Source Repository:** github.com/brocla/rust-tumbler
- **Origin Verification:** SignPath verifies that signed binaries originate from the official repository

### What will be signed

Signed binaries are those built from this repository's own source:

| Artifact | Signed |
|---|---|
| `Tumbler.exe` | Yes |
| `tumbler-thumbnailer.dll` (Explorer thumbnail handler) | Yes |
| NSIS installer (`*-setup.exe`) | Yes |
| MSI installer (`*.msi`) | Yes |
| `pdfium.dll`, `qpdf.dll`, MSVC runtime DLLs | No — bundled upstream libraries, redistributed as published |

## Team Roles

Tumbler has a single maintainer, who holds all three roles.

| Role | Person | Responsibility |
|---|---|---|
| **Author** | [@brocla](https://github.com/brocla) | Maintains source code |
| **Reviewer** | [@brocla](https://github.com/brocla) | Reviews external contributions |
| **Approver** | [@brocla](https://github.com/brocla) | Approves signing requests |

## Security Practices

- Private signing keys are stored in SignPath's Hardware Security Module (HSM)
- All signing requests require manual approval
- Binaries are verified to originate from the official GitHub repository
- Timestamps ensure signatures remain valid even after certificate expiration
- All team members use multi-factor authentication on GitHub and SignPath

## Reporting Issues

If you encounter a signed binary that appears malicious or tampered with:

1. **Do not run the file**
2. Report to: tumbler-contact@keywind.cc
3. Include the file hash (SHA256) and download source

## Other Platforms

Tumbler is a Windows-only application; no other platform builds are published.

---

Free code signing provided by [SignPath.io](https://signpath.io/), certificate
by [SignPath Foundation](https://signpath.org/).
