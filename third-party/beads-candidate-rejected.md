# Rejected Beads candidate: `0a5cb04`

The initially proposed Beads Rust source was reviewed at
`0a5cb04fdcd4f9a11c13de0a05084146c6c393ef`. Its source package declared MIT
and its `Cargo.lock` SHA-256 was
`0b958a478b56ca529a0e3779a4b6df60baae61f32262b7a24a0911e480dafaaf`.

It is rejected for Orchard packaging. The complete lockfile metadata review
found 446 packages and restrictive license riders in `asupersync 0.2.5`,
`franken-decision 0.2.5`, `franken-evidence 0.2.5`, `franken-kernel 0.2.5`,
and the `fsqlite 0.1.0` family. That candidate and its temporary metadata dump
were discarded. The approved dependency inventory is recorded relative to this
repository in `beads-legacy-license-inventory.tsv`, and its full license texts
are in `BEADS_THIRD_PARTY_LICENSES.txt`.

No output hash is recorded because the candidate must not be adopted or bundled.
The previous locked build was stopped after this license result. A distinct
earlier candidate is under separate review; it is not a substitution for this
rejected source until its own lockfile and license evidence are complete.
