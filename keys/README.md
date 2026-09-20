# Splitlane signing keys

This directory holds public-key material for verifying Splitlane release
artifacts.

## `splitlane-release.asc`

ASCII-armored OpenPGP public key used to sign `.deb` and `.rpm` release
artifacts and, once a package repository exists, its apt/dnf metadata.

- **Fingerprint:** `F87D 58DD 68A5 1E30 4ADD  0B92 3544 5D59 1132 AB3D`
- **Algorithm / size:** RSA 4096
- **User ID:** `Splitlane Release <ivan@topgun.build>`
- **Expires:** 2028-09-15

## Verifying the key before trusting it

Do not import this file into a keyring before checking its fingerprint -
importing first defeats the check.

```sh
gpg --show-keys --with-fingerprint keys/splitlane-release.asc
```

The fingerprint printed must match the one above character for character,
and the same fingerprint must appear in the GitHub release notes you
downloaded the artifact from. If either differs, stop.

Then verify an artifact:

```sh
gpg --import keys/splitlane-release.asc
rpm --import keys/splitlane-release.asc && rpm -K splitlane-*.rpm   # RPM
dpkg-sig --verify splitlane_*.deb                                  # .deb
```

## `keys/` and `packaging/`

This file is byte-identical to
[`packaging/splitlane-release.asc`](../packaging/splitlane-release.asc),
which the `.deb` installs as `/usr/share/keyrings/splitlane-archive.asc` and
the package-repository workflow signs with. `keys/` is the path for people;
`packaging/` is the path the build reads. **Both files must stay in sync** -
a key rotation updates both in the same commit.
