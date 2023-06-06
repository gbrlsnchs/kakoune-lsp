use std::collections::HashMap;
use std::collections::HashSet;

use crate::capabilities::attempt_server_capability;
use crate::capabilities::CAPABILITY_CODE_ACTIONS;
use crate::capabilities::CAPABILITY_CODE_ACTIONS_RESOLVE;
use crate::context::*;
use crate::position::*;
use crate::types::*;
use crate::util::*;
use crate::wcwidth;
use indoc::formatdoc;
use itertools::Itertools;
use lazy_static::lazy_static;
use lsp_types::request::*;
use lsp_types::*;
use serde::Deserialize;
use url::Url;

pub fn text_document_code_action(meta: EditorMeta, params: EditorParams, ctx: &mut Context) {
    let eligible_servers: Vec<_> = ctx
        .language_servers
        .iter()
        .filter(|srv| attempt_server_capability(*srv, &meta, CAPABILITY_CODE_ACTIONS))
        .collect();
    if meta.fifo.is_none() && eligible_servers.is_empty() {
        return;
    }

    let params = CodeActionsParams::deserialize(params)
        .expect("Params should follow CodeActionsParams structure");

    let document = match ctx.documents.get(&meta.buffile) {
        Some(document) => document,
        None => {
            let err = format!("Missing document for {}", &meta.buffile);
            error!("{}", err);
            if !meta.hook {
                ctx.exec(meta, format!("lsp-show-error '{}'", &editor_escape(&err)));
            }
            return;
        }
    };
    let ranges = eligible_servers
        .into_iter()
        .map(|(language_id, srv_settings)| {
            (
                language_id.clone(),
                kakoune_range_to_lsp(
                    &parse_kakoune_range(&params.selection_desc).0,
                    &document.text,
                    srv_settings.offset_encoding,
                ),
            )
        })
        .collect();
    code_actions_for_range(meta, params, ctx, ranges)
}

fn code_actions_for_range(
    meta: EditorMeta,
    params: CodeActionsParams,
    ctx: &mut Context,
    ranges: HashMap<LanguageId, Range>,
) {
    let buff_diags = ctx.diagnostics.get(&meta.buffile);

    let diagnostics: HashMap<LanguageId, Vec<Diagnostic>> = if let Some(buff_diags) = buff_diags {
        buff_diags
            .iter()
            .filter(|(language_id, d)| ranges_overlap(d.range, ranges[language_id]))
            .cloned()
            .fold(HashMap::new(), |mut m, v| {
                let (language_id, diagnostic) = v;
                m.entry(language_id).or_default().push(diagnostic);
                m
            })
    } else {
        HashMap::new()
    };

    let req_params = ranges
        .into_iter()
        .map(|(language_id, range)| {
            (
                language_id,
                vec![CodeActionParams {
                    text_document: TextDocumentIdentifier {
                        uri: Url::from_file_path(&meta.buffile).unwrap(),
                    },
                    range,
                    context: CodeActionContext {
                        diagnostics: diagnostics.remove(&language_id).unwrap_or_default(),
                        only: None,
                        trigger_kind: Some(if meta.hook {
                            CodeActionTriggerKind::AUTOMATIC
                        } else {
                            CodeActionTriggerKind::INVOKED
                        }),
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                }],
            )
        })
        .collect();
    ctx.call::<CodeActionRequest, _>(
        meta,
        RequestParams::Each(req_params),
        move |ctx: &mut Context, meta, results| {
            editor_code_actions(meta, results, ctx, params, ranges)
        },
    );
}

fn editor_code_actions(
    meta: EditorMeta,
    results: Vec<(LanguageId, Option<CodeActionResponse>)>,
    ctx: &mut Context,
    params: CodeActionsParams,
    mut ranges: HashMap<LanguageId, Range>,
) {
    if !meta.hook
        && results
            .iter()
            .all(|(language_id, result)| match ranges.get(language_id) {
                Some(range) => {
                    result == &Some(vec![])
                        && range.start.character != 0
                        && range.end.character != EOL_OFFSET
                }
                // Range is not registered for the language server,
                // so let's not let it influence in whether we should
                // reset the range and re-run code actions.
                None => true,
            })
    {
        // Some servers send code actions only if the requested range includes the affected
        // AST nodes.  Let's make them more convenient to access by requesting on whole lines.
        for (_, range) in &mut ranges {
            range.start.character = 0;
            range.end.character = EOL_OFFSET;
        }
        code_actions_for_range(meta, params, ctx, ranges);
        return;
    }

    let actions: Vec<_> = results
        .into_iter()
        .flat_map(|(language_id, cmd)| {
            let cmd: Vec<_> = cmd
                .unwrap_or_default()
                .into_iter()
                .map(|cmd| (language_id, cmd))
                .collect();
            cmd
        })
        .collect();

    for (_, cmd) in &actions {
        match cmd {
            CodeActionOrCommand::Command(cmd) => info!("Command: {:?}", cmd),
            CodeActionOrCommand::CodeAction(action) => info!("Action: {:?}", action),
        }
    }

    let may_resolve: HashSet<_> = ranges
        .iter()
        .filter(|(language_id, _)| {
            let language_id = *language_id;
            let srv_settings = &ctx.language_servers[language_id];

            attempt_server_capability(
                (language_id, srv_settings),
                &meta,
                CAPABILITY_CODE_ACTIONS_RESOLVE,
            )
        })
        .map(|(language_id, _)| language_id)
        .collect();

    // TODO: Should pattern contain the server's name?
    if let Some(pattern) = params.code_action_pattern.as_ref() {
        let regex = match regex::Regex::new(pattern) {
            Ok(regex) => regex,
            Err(error) => {
                let command = format!(
                    "lsp-show-error 'invalid pattern: {}'",
                    &editor_escape(&error.to_string())
                );
                ctx.exec(meta, command);
                return;
            }
        };
        let matches = actions
            .iter()
            .filter(|(_, c)| {
                let title = match c {
                    CodeActionOrCommand::Command(command) => &command.title,
                    CodeActionOrCommand::CodeAction(action) => &action.title,
                };
                regex.is_match(title)
            })
            .collect::<Vec<_>>();
        let sync = meta.fifo.is_some();
        let fail = if sync {
            // We might be running from a hook, so let's allow silencing errors with a "try".
            // Also, prefix with the (presumable) function name, to reduce confusion.
            "fail lsp-code-action:"
        } else {
            "lsp-show-error"
        }
        .to_string();
        let command = match matches.len() {
            0 => fail + " 'no matching action available'",
            1 => {
                let (language_id, cmd) = matches[0];
                let may_resolve = may_resolve.contains(language_id);
                code_action_or_command_to_editor_command(cmd, sync, may_resolve)
            }
            _ => fail + " 'multiple matching actions'",
        };
        ctx.exec(meta, command);
        return;
    }

    let titles_and_commands = actions
        .iter()
        .map(|(language_id, c)| {
            let mut title: &str = match c {
                CodeActionOrCommand::Command(command) => &command.title,
                CodeActionOrCommand::CodeAction(action) => &action.title,
            };
            if let Some((head, _)) = title.split_once('\n') {
                title = head
            }
            let may_resolve = may_resolve.contains(language_id);
            let select_cmd = code_action_or_command_to_editor_command(c, false, may_resolve);
            format!("{} {}", editor_quote(title), editor_quote(&select_cmd))
        })
        .join(" ");

    #[allow(clippy::collapsible_else_if)]
    let command = if params.perform_code_action {
        if actions.is_empty() {
            "lsp-show-error 'no actions available'".to_string()
        } else {
            format!("lsp-perform-code-action {}\n", titles_and_commands)
        }
    } else {
        if actions.is_empty() {
            "lsp-hide-code-actions\n".to_string()
        } else {
            lazy_static! {
                static ref CODE_ACTION_INDICATOR: &'static str =
                    wcwidth::expected_width_or_fallback("💡", 2, "[A]");
            }
            let commands = formatdoc!(
                "set-option global lsp_code_action_indicator {}
                 lsp-show-code-actions {}
                 ",
                *CODE_ACTION_INDICATOR,
                titles_and_commands
            );
            format!("evaluate-commands -- {}", editor_quote(&commands))
        }
    };
    ctx.exec(meta, command);
}

fn code_action_or_command_to_editor_command(
    action: &CodeActionOrCommand,
    sync: bool,
    may_resolve: bool,
) -> String {
    match action {
        CodeActionOrCommand::Command(command) => execute_command_editor_command(command, sync),
        CodeActionOrCommand::CodeAction(action) => {
            code_action_to_editor_command(action, sync, may_resolve)
        }
    }
}

fn code_action_to_editor_command(action: &CodeAction, sync: bool, may_resolve: bool) -> String {
    let command = match &action.command {
        Some(command) => "\n".to_string() + &execute_command_editor_command(command, sync),
        None => "".to_string(),
    };
    match &action.edit {
        Some(edit) => apply_workspace_edit_editor_command(edit, sync) + &command,
        None => {
            if may_resolve {
                let args = &serde_json::to_string(&action).unwrap();
                format!("lsp-code-action-resolve-request {}", editor_quote(args))
            } else {
                command
            }
        }
    }
}

pub fn apply_workspace_edit_editor_command(edit: &WorkspaceEdit, sync: bool) -> String {
    // Double JSON serialization is performed to prevent parsing args as a TOML
    // structure when they are passed back via lsp-apply-workspace-edit.
    let edit = &serde_json::to_string(edit).unwrap();
    let edit = editor_quote(&serde_json::to_string(&edit).unwrap());
    format!(
        "{} {}",
        if sync {
            "lsp-apply-workspace-edit-sync"
        } else {
            "lsp-apply-workspace-edit"
        },
        edit
    )
}

pub fn execute_command_editor_command(command: &Command, sync: bool) -> String {
    let cmd = editor_quote(&command.command);
    // Double JSON serialization is performed to prevent parsing args as a TOML
    // structure when they are passed back via lsp-execute-command.
    let args = &serde_json::to_string(&command.arguments).unwrap();
    let args = editor_quote(&serde_json::to_string(&args).unwrap());
    format!(
        "{} {} {}",
        if sync {
            "lsp-execute-command-sync"
        } else {
            "lsp-execute-command"
        },
        cmd,
        args
    )
}

pub fn text_document_code_action_resolve(
    meta: EditorMeta,
    params: EditorParams,
    ctx: &mut Context,
) {
    let params = CodeActionResolveParams::deserialize(params)
        .expect("Params should follow CodeActionResolveParams structure");
    let req_params = serde_json::from_str(&params.code_action).unwrap();

    ctx.call::<CodeActionResolveRequest, _>(
        meta,
        RequestParams::All(vec![req_params]),
        move |ctx: &mut Context, meta, results| {
            if let Some((_, result)) = results.first() {
                let cmd = code_action_to_editor_command(result, false, false);
                ctx.exec(meta, cmd)
            }
        },
    );
}
