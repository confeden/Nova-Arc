# What is vendored here, and under what terms

The LZMA2 **encoder** from the LZMA SDK 25.01 (Igor Pavlov), as bundled in the
7-Zip source distribution.

Every file in this directory carries `Igor Pavlov : Public domain` in its own
header, and 7-Zip's `7-Zip-License.txt` says exactly that those files are in
the public domain. The LGPL that covers other parts of the 7-Zip distribution
(the C++ tree, the GUI) applies to NONE of the files here, and no file from
those parts is vendored.

That is the whole reason this is vendored rather than pulled from a wrapper
crate: nova has not chosen its own licence yet (D11), and the project already
refused a 16% recompression win because it came with LGPL-3.0 (N20). A
dependency that pre-empts the owner's choice is not acceptable, so the
provenance has to be checkable in the repository itself.

Encoder only. The decoder is `lzma-rust2`, pure Rust, and what this writes is
an ordinary LZMA2 stream that it reads back byte for byte.
