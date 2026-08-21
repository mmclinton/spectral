# Third-Party Notices

Spectral is licensed under the GNU GPL v3.0 only (see [LICENSE](LICENSE)).
Its binaries statically link the components below. The BSD-2-Clause and
BSD-3-Clause licenses require reproducing their notices with binary
distributions; this file satisfies that requirement and should ship alongside
the binaries.

Scope: every crate here is linked into a shipped binary. Crates that are
BSD-licensed but only run at build time — `bindgen`, `exr`, `lebe`, `ravif`,
`rav1e`, `av1-grain`, `v_frame`, `avif-serialize`, `num_enum` — reach the
graph through `i-slint-compiler` and carry no binary-distribution duty.
Crates offering BSD as one option among several — `zerocopy`, `moxcms`,
`pxfm` — are taken under their Apache-2.0 arm.

## nnnoiseless 0.5.1 — BSD-3-Clause

Rust port of the RNNoise noise-suppression library by Joe Neeman, including
the bundled pre-trained RNNoise model weights (RNNoise by Jean-Marc Valin,
Mozilla / Xiph.Org Foundation).

- <https://github.com/jneem/nnnoiseless>
- RNNoise: <https://github.com/xiph/rnnoise>

```text
Copyright (c) 2020, Joe Neeman
Copyright (c) 2017, Mozilla
Copyright (c) 2007-2017, Jean-Marc Valin
Copyright (c) 2005-2017, Xiph.Org Foundation
Copyright (c) 2003-2004, Mark Borgerding

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

- Redistributions of source code must retain the above copyright
notice, this list of conditions and the following disclaimer.

- Redistributions in binary form must reproduce the above copyright
notice, this list of conditions and the following disclaimer in the
documentation and/or other materials provided with the distribution.

- Neither the name of the Xiph.Org Foundation nor the names of its
contributors may be used to endorse or promote products derived from
this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
``AS IS'' AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED.  IN NO EVENT SHALL THE FOUNDATION
OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## Slint 1.17.1 — GPL-3.0-only (of its available options)

The GUI toolkit. Slint is triple-licensed (GPL-3.0-only, Slint Royalty-Free,
Slint Software License); Spectral uses it under the GPL-3.0-only option — the
same license as Spectral, whose terms in [LICENSE](LICENSE) already cover the
combined work. Listed for transparency, since Slint's licensing is a common
question; the GPL imposes no separate attribution-file obligation.

- <https://slint.dev>

## tiny-skia / tiny-skia-path — BSD-3-Clause

The 2D rasterizer behind Slint's software renderer, reached twice: 0.12.0 via
`resvg` → `i-slint-common` → `i-slint-core`, and 0.11.4 via `sctk-adwaita` →
`winit`, which draws the Wayland client-side title bar. Both versions carry
the notice below.

- <https://github.com/linebender/tiny-skia>

```text
Copyright (c) 2011 Google Inc. All rights reserved.
Copyright (c) 2020 Yevhenii Reizner All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are
met:

  * Redistributions of source code must retain the above copyright
    notice, this list of conditions and the following disclaimer.

  * Redistributions in binary form must reproduce the above copyright
    notice, this list of conditions and the following disclaimer in
    the documentation and/or other materials provided with the
    distribution.

  * Neither the name of the copyright holder nor the names of its
    contributors may be used to endorse or promote products derived
    from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## arrayref 0.3.9 — BSD-2-Clause

Fixed-size array references, used by `tiny-skia` 0.12.0.

- <https://github.com/droundy/arrayref>

```text
Copyright (c) 2015 David Roundy <roundyd@physics.oregonstate.edu>
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are
met:

1. Redistributions of source code must retain the above copyright
   notice, this list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright
   notice, this list of conditions and the following disclaimer in the
   documentation and/or other materials provided with the
   distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## Other dependencies

The remaining statically linked Rust dependencies are MIT- and/or
Apache-2.0-licensed (see `Cargo.lock` for the full inventory), neither of
which requires a per-crate notice here. Their notices are not individually
reproduced; a generated full inventory (e.g. via `cargo-about`, restricted to
the normal-dependency graph for the two `[graph] targets` in `deny.toml`)
would replace this hand-maintained tail and is the durable fix.

To re-derive the list above after a dependency change:

```
cargo deny list -f json          # group crates by license atom
cargo tree -e normal --target x86_64-unknown-linux-gnu -i <crate>
```

A crate needs a section here only when its license expression offers no
non-BSD option *and* a normal-dependency path reaches a shipped binary.
