# kb-agent

The command. Conversion itself lives in
[`pdf-extractor`](../pdf-extractor); everything here is what a library should
not decide — what the flags are called, where output goes, and what gets said
on the way.

```
kb-agent <input.pdf> [OPTIONS]

  -o, --output <file>   Write Markdown here (default: <input> with a .md extension)
      --stdout          Write Markdown to stdout instead of a file
      --pages <list>    Only convert these 1-indexed pages, e.g. 1,4,7-9
      --page-markers    Insert <!-- Page N --> markers between pages
      --images          Include ![Image: ...] placeholders
      --keep-furniture  Keep repeated headers/footers and page numbers
      --compact         Token-efficient output instead of source fidelity
      --password <pw>   Password for an encrypted PDF
  -h, --help            Show this help
  -V, --version         Show the version
```

Markdown goes to the file; a plan line, a summary line, and any warnings go to
stderr, so `--stdout` stays pipeable. The program refuses to write over its own
input, and checks that before starting rather than after.

## OCR

OCR is on by default and costs nothing on a PDF that does not need it.

```
      --ocr <mode>      auto (default; only the pages that need it),
                        off (text layer only, never OCR),
                        or force (every page, ignoring any text layer)
      --ocr-dpi <n>     Page render resolution (default: 300)
      --ocr-min-confidence <n>   Drop OCR spans below this 0-1 score (default: 0)
      --ocr-model-dir <dir>      Use models from here instead of the cache
      --ocr-offline     Fail rather than download missing models
      --ocr-jobs <n>    OCR worker processes (default: sized to this machine)
```

A run that will use OCR says so up front, with a page count and a rough
estimate, then reports progress as it goes:

```
$ ./target/release/kb-agent samples/book.pdf
samples/book.pdf: 595 page(s), Scanned (confidence 0.95)
OCR: 595 page(s) at 300 DPI on CPU across 7 workers, about 2 min — the ~31 MB model set downloads on first use
OCR: page 300 of 595
done: 595 page(s), 595 OCR'd — render 53.5 s, recognize 24 min 9 s across 7 workers, total 3 min 41 s
wrote samples/book.md (1620659 bytes)
```

Recognition time is summed across workers, so on a parallel run it is larger
than the wall-clock total rather than smaller. The
[library README](../pdf-extractor/README.md#ocr) explains why the run is shaped
this way, and what the DPI and job count actually cost.

## Being a worker

`main` gives the library first refusal on the process:

```rust
if let Some(code) = pdf_extractor::run_worker_if_spawned() {
    return code;
}
```

An OCR worker is this same executable, re-invoked with no arguments, so that
line has to come before the command line is parsed. Without it there is no
fan-out — see [`run_worker_if_spawned`](../pdf-extractor/README.md#being-a-host).
