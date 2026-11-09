//! Ask the user for values, with a flag for every question.
//!
//! Each helper takes the value from its flag if one was given. Otherwise, in
//! a terminal, it asks, re-asking until the answer is valid. When input
//! isn't a terminal (a script, CI), it never asks: a missing required value
//! is an error naming the flag to use.
//!
//! Every answer goes through the same `parse` function as the flag, so an
//! interactive answer and a flag are validated identically.

use std::io::IsTerminal;

use dialoguer::{Confirm, Input};

use crate::error::Error;

/// Whether gitaway can ask questions: both stdin and stderr (where prompts
/// are drawn) must be a terminal.
pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// A value that must be provided.
pub fn required<T>(
    flag: Option<String>,
    flag_name: &str,
    question: &str,
    parse: impl Fn(&str) -> Result<T, String>,
) -> Result<T, Error> {
    if let Some(value) = flag {
        return parse(&value).map_err(|e| Error::msg(format!("{flag_name}: {e}")));
    }
    if !interactive() {
        return Err(Error::msg(format!(
            "{flag_name} is required when not running in a terminal"
        )));
    }
    let answer: String = Input::new()
        .with_prompt(question)
        .validate_with(|input: &String| parse(input).map(|_| ()))
        .interact_text()
        .map_err(prompt_error)?;
    parse(&answer).map_err(Error::msg)
}

/// A value that may be left out. Blank answers and non-interactive runs
/// without the flag give `None`.
pub fn optional<T>(
    flag: Option<String>,
    flag_name: &str,
    question: &str,
    parse: impl Fn(&str) -> Result<T, String>,
) -> Result<Option<T>, Error> {
    if let Some(value) = flag {
        return parse(&value)
            .map(Some)
            .map_err(|e| Error::msg(format!("{flag_name}: {e}")));
    }
    if !interactive() {
        return Ok(None);
    }
    let answer: String = Input::new()
        .with_prompt(format!("{question} (blank to skip)"))
        .allow_empty(true)
        .validate_with(|input: &String| {
            if input.trim().is_empty() {
                Ok(())
            } else {
                parse(input).map(|_| ())
            }
        })
        .interact_text()
        .map_err(prompt_error)?;
    if answer.trim().is_empty() {
        Ok(None)
    } else {
        parse(&answer).map(Some).map_err(Error::msg)
    }
}

/// Asks a yes/no question, defaulting to no. Not interactive means no.
pub fn confirm(question: &str) -> Result<bool, Error> {
    if !interactive() {
        return Ok(false);
    }
    Confirm::new()
        .with_prompt(question)
        .default(false)
        .interact()
        .map_err(prompt_error)
}

fn prompt_error(e: dialoguer::Error) -> Error {
    Error::msg(format!("couldn't read your answer: {e}"))
}
