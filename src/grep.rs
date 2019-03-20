use std::io;
use std::io::{Read, Write};
use std::fs::File;

use regex::bytes::{Regex, RegexBuilder};

use crate::args::{self, Args};


/// Build the regex pattern with the given options.
/// By default, the `unicode` flag is set to false, and `dot_matches_new_line` set to true.
fn build_pattern(
  pattern: &String,
  options: &args::Options
) -> Result<Regex, regex::Error> {
  let mut builder = RegexBuilder::new(pattern);

  builder.unicode(false);
  builder.dot_matches_new_line(true);
  builder.case_insensitive(options.case_insensitive);

  builder.build()
}


/// Run bgrep, outputting `path` to the given `StdoutLock` if there is a match.
/// Returns whether there was a match.
fn grep_filename(
  stdout: &mut io::StdoutLock,
  options: &args::Options,
  path: &str,
  pattern: &Regex,
  buffer: &[u8]
) -> io::Result<bool> {
  // When inverse matching, matches must be checked until a "hole" is found.
  // Otherwise, the more performant `Regex::is_match` can be used.
  if options.inverse {
    // if the pattern matches multiple times, comprising the entire buffer, then no
    // inverse match is present.
    let mut matches = pattern.find_iter(buffer);

    let mut end = 0; // Start from the beginning of the buffer.

    // Try to find a "hole" between matches:
    let inverse_match = matches.find(
      |m| {
        let matched = m.start() > end;

        end = m.end();

        matched
      }
    );

    // Also check for a "hole" after the last match.
    let matched = (inverse_match.is_some() || end < buffer.len())
                ^ options.non_matching; // List non matching files.

    if matched {
      writeln!(stdout, "{}", path)?;
    }

    Ok(matched)
  }
  else {
    let matched = pattern.is_match(buffer) ^ options.non_matching;

    if matched {
      writeln!(stdout, "{}", path)?;
    }

    Ok(matched)
  }
}


/// Run bgrep, outputting the matched bytes to the given `StdoutLock`.
/// Returns whether there was a match.
fn grep_bytes(
  stdout: &mut io::StdoutLock,
  options: &args::Options,
  pattern: &Regex,
  buffer: &[u8]
) -> io::Result<bool> {
  let mut write_bytes = |bs| {
    stdout.write(bs)?;
    writeln!(stdout)
  };


  let mut matched = false;

  if options.inverse {
    // `Regex::split` yields the slices outside the matches.
    let mut matches = pattern.split(buffer);

    // Set `matched` if there is a first occurrence:
    if let Some(bs) = matches.next() {
      if !bs.is_empty() { // A regex may have a empty match, but when inverse matching 
        write_bytes(bs)?; // we disconsider empty intervals.
        matched = true;
      }
    };

    // Iterate the remaining matches:
    for bs in matches {
      if !bs.is_empty() {
        write_bytes(bs)?;
      }
    }
  }
  else {
    let mut matches = pattern.find_iter(buffer);

    // Set `matched` if there is a first occurrence:
    if let Some(m) = matches.next() {
      write_bytes(m.as_bytes())?;
      matched = true;
    }

    // Iterate the remaining matches:
    for m in matches {
      write_bytes(m.as_bytes())?;
    }
  };


  Ok(matched)
}


/// Run bgrep, outputting the matche's offset in hex to the given `StdoutLock`.
/// Returns whether there was a match.
fn grep_offset(
  stdout: &mut io::StdoutLock,
  options: &args::Options,
  pattern: &Regex,
  buffer: &[u8]
) -> io::Result<bool> {
  let mut write_hex = |x| writeln!(stdout, "0x{:x}", x);


  let mut matches = pattern.find_iter(buffer);

  let mut matched = false;

  if options.inverse {
    // if the pattern matches multiple times, comprising the entire buffer, then no
    // inverse match is present.
    let mut end = 0; // Start from the beginning of the buffer.

    for m in matches {
      if m.start() > end {
        write_hex(end)?;
        matched = true;
      }

      end = m.end()
    }

    if end < buffer.len() { // Also check for a "hole" after the last match.
      write_hex(end)?;
      matched = true;
    }
  }
  else {
    // Set `matched` if there is a first occurrence:
    if let Some(m) = matches.next() {
      write_hex(m.start())?;
      matched = true;
    }

    // Iterate the remaining matches:
    for m in matches {
      write_hex(m.start())?;
    }
  }


  Ok(matched)
}


/// Run bgrep with the given args, outputting to stdout.
/// Error detail may be outputted to stderr.
/// Returns whether there was a match.
pub fn run(args: Args) -> io::Result<bool> {
  // Deconstruct to split ownership.
  let Args { options, pattern, files } = args;


  let pattern = build_pattern(&pattern, &options).map_err(
    |e| {
      eprintln!("Error: invalid pattern '{}', {}", pattern, e);
      io::ErrorKind::InvalidInput
    }
  )?;


  // Lock stdout before loop to prevent locking repetitively.
  let stdout = io::stdout();
  let mut stdout = stdout.lock();

  // Reuse the same buffer for all the files, minimizing allocations.
  let mut buffer = Vec::<u8>::new();

  // We need to store the last generated error if any, or whether there was a match.
  // Converting to vec to use the owned iterator. Box<[T]> has no owned iterator.
  files.into_vec().into_iter().fold(
    Ok(false), // : io::Result<bool>, whether there was a match, or the last error.
    |result: io::Result<bool>, path: String| {
      buffer.clear();

      let (read_result, path) =
        if path == "-" {
          (io::stdin().lock().read_to_end(&mut buffer), "<stdin>")
        }
        else {
          let mut file = File::open(&path)
                              .map_err(|e| {
                                eprintln!("Error: failed to open file '{}'", path);
                                e
                              })?;

          // Resize buffer to the file size if it exceeds the current size.
          // Currently, the strategy is to grow if needed, and otherwise do nothing.
          // Considering we never shrink the buffer, this can be bad if the first file
          // is huge and the others are small.
          let file_size = file.metadata()
                              .map(|m| m.len())
                              .unwrap_or(0) as usize;
          buffer.reserve(
            file_size.saturating_sub(buffer.len())
          );

          (file.read_to_end(&mut buffer), path.as_str())
        };


      if let Err(e) = read_result {
        eprintln!("Error: failed to read file '{}'", path);
        return Err(e);
      }


      // Trim the ending newline if requested and present:
      if options.trim_ending_newline && buffer.last() == Some(&b'\n') {
        buffer.pop();
      };


      let matched = match options.output {
        args::Output::FileName => grep_filename(&mut stdout, &options, &path, &pattern, &buffer),
        args::Output::Bytes    => grep_bytes(&mut stdout, &options, &pattern, &buffer),
        args::Output::Offset   => grep_offset(&mut stdout, &options, &pattern, &buffer)
      }?;


      // Preserve the last error or matched flag:
      if matched {
        result.and(Ok(true))
      }
      else {
        result
      }
    }
  )
}
