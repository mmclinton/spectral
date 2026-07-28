# Third-Party Notices

Spectral is licensed under the GNU GPL v3.0 only (see [LICENSE](LICENSE)).
Its binaries statically link the components below. The BSD-3-Clause license
requires reproducing its notice with binary distributions; this file
satisfies that requirement and should ship alongside the binaries.

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

## Slint 1.15.0 — GPL-3.0-only (of its available options)

The GUI toolkit. Slint is triple-licensed (GPL-3.0-only, Slint Royalty-Free,
Slint Software License); Spectral uses it under the GPL-3.0-only option — the
same license as Spectral, whose terms in [LICENSE](LICENSE) already cover the
combined work. Listed for transparency, since Slint's licensing is a common
question; the GPL imposes no separate attribution-file obligation.

- <https://slint.dev>

## JetBrains Mono 2.304 — OFL-1.1 (Flatpak builds only)

The interface is laid out for a monospaced face. Builds that cannot rely on
the host providing one bundle the font instead of hoping for it: the Flatpak
ships four faces under `/app/share/fonts/jetbrains-mono` and installs the
OFL text beside this file. Nothing is linked — the font is data the renderer
loads at runtime — and no other packaging target bundles it.

- <https://github.com/JetBrains/JetBrainsMono>

## Other dependencies

The remaining statically linked Rust dependencies are MIT- and/or
Apache-2.0-licensed (see `Cargo.lock` for the full inventory). Their notices
are not individually reproduced here; a generated full inventory (e.g. via
`cargo-about`) is a possible follow-up.
