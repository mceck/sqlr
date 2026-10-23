# Third-party licenses

sqlr is MIT-licensed (see [LICENSE.md](LICENSE.md)). This file covers the
third-party assets bundled with the source and the app.

## Icons

The database icons come from the [Iconify catalog](https://icon-sets.iconify.design/).
Each set keeps its own license. SVG files are in `crates/sqlr-ui/assets/icons/svg/`;
the `.rgba` files are in `crates/sqlr-ui/assets/icons/`.

### Devicon — MIT

Icons: Elasticsearch, MongoDB, SQL Server, MySQL, Redis, SQLite.

Copyright (c) 2015 konpa  
Source: <https://github.com/devicons/devicon>  
License: MIT (text below)  
Changes: converted to RGBA for the app; `mysql_dark.rgba` is recolored.

### selfh.st/icons — CC BY 4.0

Icons: ClickHouse, Oracle.

Author: selfh.st  
Source: <https://github.com/selfhst/icons>  
License: <https://creativecommons.org/licenses/by/4.0/>  
Changes: converted to RGBA for the app; the SVG files carry this notice.  
No endorsement by selfh.st is implied.

### SVG Logos — CC0 1.0

Icons: PostgreSQL, SAP.

Author: Gil Barbara  
Source: <https://github.com/gilbarbara/logos>  
License: <https://creativecommons.org/publicdomain/zero/1.0/>

No attribution is required; this notice is included for provenance.

`database.rgba` is original to sqlr.

## Fonts

- **Inter** — SIL Open Font License 1.1. Copyright (c) 2016 The Inter Project
  Authors. Full text:
  [`crates/sqlr-ui/assets/fonts/OFL-Inter.txt`](crates/sqlr-ui/assets/fonts/OFL-Inter.txt).
- **Phosphor Icons** (via `egui-phosphor`) — MIT. Copyright (c) 2020-2021
  Phosphor Icons. Source: <https://github.com/phosphor-icons/web>.

## MIT License text

The following text applies to Devicon and Phosphor Icons:

```text
MIT License

Copyright (c) 2015 konpa
Copyright (c) 2020-2021 Phosphor Icons

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Trademarks

PostgreSQL, MySQL, Oracle, SQL Server, SAP HANA, MongoDB, Redis, Elasticsearch,
SQLite, ClickHouse and their logos are trademarks or registered trademarks of
their respective owners. They are used only to identify the databases supported
by sqlr. No affiliation or endorsement is implied.

The licenses above grant copyright permissions only, not trademark rights.

## Rust crates

Dependencies are pinned in `Cargo.lock`. Their licenses include MIT,
Apache-2.0, BSD, ISC, Zlib, MPL-2.0, Unicode-3.0, OFL-1.1, Ubuntu-font-1.0,
CC0-1.0, Unlicense, BSL-1.0 and CDLA-Permissive-2.0. No GPL-only dependency is
required.

`nucleo-matcher` and `option-ext` are MPL-2.0; their source is available from
crates.io. Include the full dependency license texts in binary releases.
