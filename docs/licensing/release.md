# Release Checklist

Before any public release, complete all items:

## Legal / provenance

- [ ] `LICENSE-MIT` present
- [ ] `LICENSE-APACHE` present  
- [ ] `COPYRIGHT` updated
- [ ] `NOTICE` updated
- [ ] `docs/licensing/provenance.md` fully updated
- [ ] All dependencies audited (`cargo deny check`)
- [ ] Third-party licenses verified
- [ ] No external runtime source accidentally incorporated
- [ ] Source provenance audited for all non-trivial code
- [ ] Test fixture provenance audited
- [ ] Documentation provenance audited

## Technical

- [ ] CI green (fmt, check, test, clippy, deny)
- [ ] Spec tests pass
- [ ] Fuzz corpus clean
- [ ] Security issues reviewed (see SECURITY.md)
- [ ] SBOM generated (`cargo cyclonedx` or equivalent)
  - `scripts/sbom.py` writes a CycloneDX 1.5 SBOM from the locked graph; CI uploads it as an artifact on every run. Not yet attached to a release.
- [ ] Reproducible build verified
  - `scripts/repro-check.sh` passes locally on Windows (byte-identical CLI builds). The Linux CI job has not run yet.
- [ ] Release source archive verifiable

## Artifacts

- [ ] Source archive
- [ ] Binaries (x86_64-linux, aarch64-linux, x86_64-windows, aarch64-macos)
- [ ] SBOM
- [ ] License report
- [ ] Third-party notices
- [ ] Feature manifest (from `feature-registry.toml`)
- [ ] Build metadata (git commit, engine version, semantics version)

## Legal review

Obtain appropriate legal review before any public MIT/Apache release.
Do not rely solely on automation for licensing decisions.
