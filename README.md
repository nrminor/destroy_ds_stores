# `dds`: an optionally recursive destroyer of macos `.DS_Store` files

Developers working on MacOS can often be identified by `.DS_Store` system files that have accidentally been committed to public repositories. The best way to avoid committing these system files, of course, is to include `.DS_Store` as a line in your `.gitignore` file, as I've done for this repository.

However, wouldn't it be fun to also have an OP'd `.DS_Store` destroyer for when you need to blow off some steam? That's where `dds` comes in. It's an embarassingly parallel, optionally recursive `.DS_Store` destroyer that will show no mercy. Just change to the parent directory where you'd like to destroy all `.DS_Store`s in all subdirectories and run `dds -r`.

### Quick Start

To install, make sure you have [the Rust toolchain installed](https://www.rust-lang.org/tools/install). Next, download the source code like so:

```zsh
git clone https://github.com/nrminor/destroy_ds_stores.git
```

Then, change into it with `cd destroy_ds_stores` and run:

```zsh
cargo install --path=.
```

This will compile the tool and put `dds` on your user $PATH to make it available throughout your file system. Here's a quick look at what comes up when you run `dds -h`:

```
A command line tool that deletes the `.DS_Store` system files commonly found around MacOS filesystems. Please note that Finder may behave differently after running `dds`

Usage: dds [OPTIONS] [DIR]

Arguments:
  [DIR]  The directory to search within for `.DS_Store` files [default: .]

Options:
  -v, --verbose    Control the logging of detailed information as `dds` progresses
  -r, --recursive  Whether to search recursively in subdirectories of the provided search directory
  -d, --dry        Whether to perform a dry run where `.DS_Store` files are found but not deleted
  -h, --help       Print help
  -V, --version    Print version
```

### Disclaimer

The `.DS_Store` file does of course have a quality-of-life purpose for MacOS users: it stores configuration per-folder for how Finder should display files. If you care about that, don't use `dds`.
