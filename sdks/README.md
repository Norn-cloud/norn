# Core contracts

`norn openapi` projects the Rust IR and checker schemas into
`openapi/norn-api.openapi.json`. `packages/norn-sdk` generates TypeScript declarations
from that document. CI regenerates both and fails on drift.

`openapi-typescript` 7.13.0 is also MIT-licensed (copyright 2020 Drew Powers).
Its npm tarball SHA256 is
`127c3c93b02c3deacc93091a215235f697e6db9fe350bc50740cdaefb865d580`;
the frozen `packages/norn-sdk/bun.lock` binds its npm integrity. It emits
declarations from the Norn OpenAPI document through the package's `generate`
command. The respective generator/template sources retain these MIT notices:

MIT License

Copyright (c) 2020 Drew Powers

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
