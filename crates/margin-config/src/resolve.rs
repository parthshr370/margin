//! Keystroke resolution against a compiled table. `data/keymap-cases.jsonc` holds the cases it
//! must pass.
//!
//! For the typed keys (pending chord plus this key), bindings whose keys start with them and whose
//! context holds are grouped by their full key sequence. In each group the deepest context wins,
//! then the later-loaded binding. A winner that is an unbind ends the group with no command; a
//! winner whose command's `when` is false yields to the next. If a longer sequence would still
//! run a command, the key is pending until the next key or the chord timeout.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::{ContextNode, depth_of, holds};
use crate::keymap::{CompiledBinding, CompiledKeymap};
use crate::keystroke::is_printable;

/// The result of one key or of a chord timeout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// Run this command.
    Command {
        /// The command id.
        command: String,
        /// Its arguments, `null` when none.
        args: Value,
    },
    /// The key starts or continues a chord; wait.
    Pending,
    /// No binding: the key goes on to the widgets.
    #[serde(rename = "none")]
    NoMatch,
}

/// A key's result and the chord state after it.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// What to do now.
    pub outcome: Outcome,
    /// Keys of the chord in progress, empty when none.
    pub pending: Vec<String>,
}

struct Ranked<'a> {
    binding: &'a CompiledBinding,
    depth: usize,
    order: usize,
}

fn winner(mut group: Vec<Ranked<'_>>, stack: &[ContextNode]) -> Outcome {
    group.sort_by(|a, b| b.depth.cmp(&a.depth).then(b.order.cmp(&a.order)));
    for Ranked { binding, .. } in group {
        let Some(command) = &binding.command else { return Outcome::NoMatch };
        if holds(binding.when.as_ref(), stack) {
            return Outcome::Command { command: command.clone(), args: binding.args.clone() };
        }
    }
    Outcome::NoMatch
}

/// Applicable bindings starting with `typed`, grouped by their full key sequence (in first-seen
/// order, so results never depend on hashing).
fn candidates<'a>(table: &'a CompiledKeymap, stack: &[ContextNode], typed: &[String]) -> Vec<(Vec<String>, Vec<Ranked<'a>>)> {
    let in_text = stack.last().is_some_and(|node| node.name == "TextInput");
    let mut groups: Vec<(Vec<String>, Vec<Ranked<'a>>)> = Vec::new();
    if in_text && matches!(typed, [only] if is_printable(only)) {
        return groups;
    }
    for (order, binding) in table.bindings.iter().enumerate() {
        if !binding.keys.starts_with(typed) || (in_text && !binding.in_text) {
            continue;
        }
        let Some(depth) = depth_of(binding.context.as_ref(), stack) else { continue };
        let ranked = Ranked { binding, depth, order };
        match groups.iter_mut().find(|(keys, _)| *keys == binding.keys) {
            Some((_, group)) => group.push(ranked),
            None => groups.push((binding.keys.clone(), vec![ranked])),
        }
    }
    groups
}

/// One key after `pending`. A key that continues no binding drops the pending chord and is
/// matched on its own.
pub fn step(table: &CompiledKeymap, stack: &[ContextNode], pending: &[String], key: &str) -> Step {
    let mut typed = pending.to_vec();
    typed.push(key.to_owned());
    let groups = candidates(table, stack, &typed);
    if groups.is_empty() && !pending.is_empty() {
        return step(table, stack, &[], key);
    }
    let mut exact = Vec::new();
    let mut longer = false;
    for (keys, group) in groups {
        if keys == typed {
            exact = group;
        } else if !longer {
            longer = matches!(winner(group, stack), Outcome::Command { .. });
        }
    }
    if longer {
        return Step { outcome: Outcome::Pending, pending: typed };
    }
    Step { outcome: winner(exact, stack), pending: Vec::new() }
}

/// The chord timeout: the binding for exactly the pending keys, if one wins.
pub fn timeout(table: &CompiledKeymap, stack: &[ContextNode], pending: &[String]) -> Outcome {
    if pending.is_empty() {
        return Outcome::NoMatch;
    }
    let exact = candidates(table, stack, pending).into_iter().find(|(keys, _)| keys.as_slice() == pending);
    exact.map_or(Outcome::NoMatch, |(_, group)| winner(group, stack))
}
