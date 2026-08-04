# Releases and verification

Pushing a `v*` tag runs the release workflow against the exact tagged source.
It tests and builds native archives for:

- Linux x86-64
- macOS Apple silicon
- macOS Intel
- Windows x86-64

The publication job generates `SHA256SUMS`, verifies it before upload, creates
a GitHub artifact provenance attestation, and publishes all files in a GitHub
Release.

Standalone archives contain the `structurely` executable, README, and license;
they do not require Rust at runtime. Extract an archive, move the executable to
a directory on `PATH`, and run `structurely --version`.

## Verify a download

Verify the checksum:

```bash
sha256sum --check SHA256SUMS
```

Verify that GitHub Actions built the archive from this repository:

```bash
gh attestation verify structurely-linux-x86_64.tar.gz \
  --repo coder-company/structurely
```

The checksum protects against accidental corruption. The attestation separately
establishes the archive's build origin and workflow identity.

## Maintainer release procedure

1. Ensure `main` CI is green and the working tree is clean.
2. Update `Cargo.toml` and `Cargo.lock` to the intended version.
3. Commit the version and release notes.
4. Create and push an annotated tag:

   ```bash
   git tag -a v1.0.0 -m "Structurely v1.0.0"
   git push origin v1.0.0
   ```

5. Wait for every release matrix job and the publication job to succeed.
6. Download one archive and verify both its checksum and attestation.

Never reuse or move an existing release tag.
