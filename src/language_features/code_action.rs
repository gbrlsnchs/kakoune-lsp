use std::collections::HashMap;
use std::collections::HashSet;
use std::convert::TryFrom;
use std::mem;

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
use url::Url;

pub fn text_document_code_action(
    meta: EditorMeta,
    response_fifo: Option<ResponseFifo>,
    params: CodeActionsParams,
    ctx: &mut Context,
) {
    let eligible_servers: Vec<_> = ctx
        .servers(&meta)
        .filter(|srv| attempt_server_capability(ctx, *srv, &meta, CAPABILITY_CODE_ACTIONS))
        .collect();
    if eligible_servers.is_empty() {
        return;
    }

    let document = match ctx.documents.get(&meta.buffile) {
        Some(document) => document,
        None => {
            let err = format!("Missing document for {}", &meta.buffile);
            ctx.show_error_fifo(meta, response_fifo, err);
            return;
        }
    };
    let ranges = eligible_servers
        .into_iter()
        .map(|(server_id, server_settings)| {
            (
                server_id,
                kakoune_range_to_lsp(
                    &parse_kakoune_range(&params.selection_desc).0,
                    &document.text,
                    server_settings.offset_encoding,
                ),
            )
        })
        .collect();
    code_actions_for_ranges(meta, response_fifo, params, ctx, document.version, ranges)
}

fn code_actions_for_ranges(
    meta: EditorMeta,
    response_fifo: Option<ResponseFifo>,
    mut params: CodeActionsParams,
    ctx: &mut Context,
    version: i32,
    ranges: HashMap<ServerId, Range>,
) {
    let buff_diags = ctx.diagnostics.get(&meta.buffile);

    let mut diagnostics: HashMap<ServerId, Vec<Diagnostic>> = if let Some(buff_diags) = buff_diags {
        buff_diags
            .iter()
            .filter(|(server_id, d)| {
                ranges
                    .get(server_id)
                    .is_some_and(|r| ranges_overlap(d.range, *r))
            })
            .cloned()
            .fold(HashMap::new(), |mut m, v| {
                let (server_id, diagnostic) = v;
                m.entry(server_id).or_default().push(diagnostic);
                m
            })
    } else {
        HashMap::new()
    };

    let req_params = ranges
        .iter()
        .map(|(server_id, range)| {
            (
                *server_id,
                vec![CodeActionParams {
                    text_document: TextDocumentIdentifier {
                        uri: Url::from_file_path(&meta.buffile).unwrap(),
                    },
                    range: *range,
                    context: CodeActionContext {
                        diagnostics: diagnostics.remove(server_id).unwrap_or_default(),
                        only: match &mut params.filters {
                            Some(CodeActionFilter::ByKind(pattern)) => Some(mem::take(pattern)),
                            None | Some(CodeActionFilter::ByRegex(_)) => None,
                        },
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
        move |ctx, meta, results| {
            editor_code_actions(meta, response_fifo, results, ctx, params, version, ranges)
        },
    );
}

fn editor_code_actions(
    meta: EditorMeta,
    response_fifo: Option<ResponseFifo>,
    results: Vec<(ServerId, Option<CodeActionResponse>)>,
    ctx: &mut Context,
    params: CodeActionsParams,
    version: i32,
    mut ranges: HashMap<ServerId, Range>,
) {
    let sync = response_fifo.is_some();
    if !meta.hook
        && results
            .iter()
            .all(|(server_id, result)| match ranges.get(server_id) {
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
        // AST nodes.  Since we don't have per-line lightbulbs, let's make the common case more
        // convenient by requesting on whole lines.
        let Some(document) = ctx.documents.get(&meta.buffile) else {
            error!(ctx.to_editor(), "Missing document for {}", &meta.buffile);
            return;
        };
        if document.version != version {
            error!(
                ctx.to_editor(),
                "Stale document for {}: my ranges are for {}, document has {}",
                &meta.buffile,
                version,
                document.version
            );
            return;
        }
        for (server_id, range) in &mut ranges {
            range.start.character = 0;
            let line = document.text.line(usize::try_from(range.end.line).unwrap());
            range.end.character = kakoune_position_to_lsp(
                &KakounePosition {
                    line: range.end.line + 1,
                    column: u32::try_from(line.len_bytes()).unwrap(),
                },
                &document.text,
                ctx.server(*server_id).offset_encoding,
            )
            .character;
        }
        code_actions_for_ranges(meta, response_fifo, params, ctx, version, ranges);
        return;
    }

    let mut actions: Vec<_> = results
        .into_iter()
        .flat_map(|(server_id, cmd)| {
            let cmd: Vec<_> = cmd
                .unwrap_or_default()
                .into_iter()
                .map(|cmd| (server_id, cmd))
                .collect();
            cmd
        })
        .collect();

    for (_, cmd) in &actions {
        match cmd {
            CodeActionOrCommand::Command(cmd) => debug!(ctx.to_editor(), "Command: {:?}", cmd),
            CodeActionOrCommand::CodeAction(action) => {
                debug!(ctx.to_editor(), "Action: {:?}", action)
            }
        }
    }

    let may_resolve: HashSet<ServerId> = ranges
        .iter()
        .filter(|(server_id, _)| {
            let server_id = **server_id;
            let server_settings = ctx.server(server_id);

            attempt_server_capability(
                ctx,
                (server_id, server_settings),
                &meta,
                CAPABILITY_CODE_ACTIONS_RESOLVE,
            )
        })
        .map(|(server_id, _)| *server_id)
        .collect();

    if sync || matches!(params.filters, Some(CodeActionFilter::ByRegex(_))) {
        let actions = if let Some(CodeActionFilter::ByRegex(pattern)) = &params.filters {
            let regex = match regex::Regex::new(pattern) {
                Ok(regex) => regex,
                Err(error) => {
                    ctx.show_error_fifo(meta, response_fifo, format!("invalid pattern: {}", error));
                    return;
                }
            };
            actions
                .into_iter()
                .filter(|(_, c)| {
                    let title = match c {
                        CodeActionOrCommand::Command(command) => &command.title,
                        CodeActionOrCommand::CodeAction(action) => &action.title,
                    };
                    regex.is_match(title)
                })
                .collect::<Vec<_>>()
        } else {
            actions
        };
        let error_message = match actions.len() {
            0 => "lsp-code-actions: no matching action available",
            1 => {
                let (server_id, cmd) = &actions[0];
                let may_resolve = may_resolve.contains(server_id);
                let server_name = &ctx.server(*server_id).name;
                let command =
                    code_action_or_command_to_editor_command(server_name, cmd, sync, may_resolve);
                ctx.exec_fifo(
                    meta,
                    response_fifo,
                    format!("evaluate-commands -- {}", &editor_quote(&command)),
                );
                return;
            }
            _ => "lsp-code-actions: multiple matching actions",
        };
        ctx.show_error_fifo(meta, response_fifo, error_message);
        return;
    }

    assert!(response_fifo.is_none());

    actions.sort_by_key(|(_server, ca)| {
        // TODO Group by server?
        let empty = CodeActionKind::EMPTY;
        let kind = match ca {
            CodeActionOrCommand::Command(_) => &empty,
            CodeActionOrCommand::CodeAction(action) => action.kind.as_ref().unwrap_or(&empty),
        };
        // TODO These loosely follow what VSCode does, we should be more accurate.
        match kind.as_str() {
            "quickfix" => 0,
            "refactor" => 1,
            "refactor.extract" => 2,
            "refactor.inline" => 3,
            "refactor.rewrite" => 4,
            "source" => 5,
            "source.organizeImports" => 6,
            _ => 7,
        }
    });
    let titles_and_commands = if params.auto_single {
        "-auto-single "
    } else {
        ""
    }
    .to_string()
        + &actions
            .iter()
            .map(|(server_id, c)| {
                let mut title: &str = match c {
                    CodeActionOrCommand::Command(command) => &command.title,
                    CodeActionOrCommand::CodeAction(action) => &action.title,
                };
                if let Some((head, _)) = title.split_once('\n') {
                    title = head
                }
                let may_resolve = may_resolve.contains(server_id);
                let server_name = &ctx.server(*server_id).name;
                let select_cmd =
                    code_action_or_command_to_editor_command(server_name, c, false, may_resolve);
                format!("{} {}", editor_quote(title), editor_quote(&select_cmd))
            })
            .join(" ");

    #[allow(clippy::collapsible_else_if)]
    let command = if params.perform_code_action {
        if actions.is_empty() {
            ctx.show_error(meta, "no actions available");
            return;
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
    server_name: &ServerName,
    action: &CodeActionOrCommand,
    sync: bool,
    may_resolve: bool,
) -> String {
    match action {
        CodeActionOrCommand::Command(command) => {
            execute_command_editor_command(server_name, command, sync)
        }
        CodeActionOrCommand::CodeAction(action) => {
            code_action_to_editor_command(server_name, action, sync, may_resolve)
        }
    }
}

fn code_action_to_editor_command(
    server_name: &ServerName,
    action: &CodeAction,
    sync: bool,
    may_resolve: bool,
) -> String {
    let command = match &action.command {
        Some(command) => {
            "\n".to_string() + &execute_command_editor_command(server_name, command, sync)
        }
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

pub fn execute_command_editor_command(
    server_name: &ServerName,
    command: &Command,
    sync: bool,
) -> String {
    let cmd = editor_quote(&command.command);
    let args = command
        .arguments
        .as_ref()
        .map(|args| serde_json::to_string(args).unwrap());
    let args = editor_quote(args.as_deref().unwrap_or_default());
    format!(
        "{} {cmd} {args} {server_name}",
        if sync {
            "lsp-execute-command-sync"
        } else {
            "lsp-execute-command"
        },
    )
}

pub fn text_document_code_action_resolve(
    meta: EditorMeta,
    params: CodeActionResolveParams,
    ctx: &mut Context,
) {
    let req_params = serde_json::from_str(&params.code_action).unwrap();

    ctx.call::<CodeActionResolveRequest, _>(
        meta,
        RequestParams::All(vec![req_params]),
        move |ctx: &mut Context, meta, results| {
            if let Some((server_id, result)) = results.first() {
                let server_name = &ctx.server(*server_id).name;
                let cmd = code_action_to_editor_command(server_name, result, false, false);
                ctx.exec(meta, format!("evaluate-commands -- {}", editor_quote(&cmd)))
            }
        },
    );
}
