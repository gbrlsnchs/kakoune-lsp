use std::collections::HashMap;

use super::code_action::execute_command_editor_command;
use crate::capabilities::CAPABILITY_CODE_LENS;
use crate::context::*;
use crate::diagnostics::gather_line_flags;
use crate::position::*;
use crate::types::*;
use crate::util::editor_quote;
use crate::util::escape_tuple_element;
use crate::wcwidth;
use crate::{capabilities::server_has_capability, markup::escape_kakoune_markup};
use indoc::formatdoc;
use itertools::Itertools;
use lazy_static::lazy_static;
use lsp_types::request::*;
use lsp_types::*;

pub fn text_document_code_lens(meta: EditorMeta, ctx: &mut Context) {
    let eligible_servers: Vec<_> = ctx
        .servers(&meta)
        .filter(|(_, server)| server_has_capability(ctx.to_editor(), server, CAPABILITY_CODE_LENS))
        .collect();
    if eligible_servers.is_empty() {
        return;
    }

    let req_params = eligible_servers
        .into_iter()
        .map(|(server_id, _server)| {
            (
                server_id,
                vec![CodeLensParams {
                    text_document: TextDocumentIdentifier {
                        uri: Url::from_file_path(&meta.buffile).unwrap(),
                    },
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                }],
            )
        })
        .collect();
    ctx.call::<CodeLensRequest, _>(
        meta,
        RequestParams::Each(req_params),
        |ctx: &mut Context, meta, results| editor_code_lens(meta, results, ctx),
    );
}

fn editor_code_lens(
    meta: EditorMeta,
    results: Vec<(ServerId, Option<Vec<CodeLens>>)>,
    ctx: &mut Context,
) {
    let mut lenses: Vec<_> = results
        .into_iter()
        .flat_map(|(server_id, v)| {
            v.unwrap_or_default()
                .into_iter()
                .map(move |v| (server_id, v))
        })
        .collect();
    lenses.sort_by_key(|(_, lens)| lens.range.start);

    let buffile = &meta.buffile;
    let document = match ctx.documents.get(buffile) {
        Some(document) => document,
        None => {
            ctx.code_lenses.remove(buffile);
            return;
        }
    };
    let version = document.version;
    let inlay_code_lenses = lenses
        .iter()
        .map(|(server_id, lens)| {
            let server = ctx.server(*server_id);
            let label = lens.command.as_ref().map_or("", |v| &v.title);
            let position =
                lsp_position_to_kakoune(&lens.range.start, &document.text, server.offset_encoding);
            let line = position.line;
            lazy_static! {
                static ref CODE_LENS_INDICATOR: &'static str =
                    wcwidth::expected_width_or_fallback("🔎", 2, "[L]");
            }

            editor_quote(&format!(
                "{line}|{{InlayCodeLens}}[{} {}] ",
                *CODE_LENS_INDICATOR,
                escape_tuple_element(&escape_kakoune_markup(label))
            ))
        })
        .join(" ");

    ctx.code_lenses.insert(meta.buffile.clone(), lenses);

    let line_flags = gather_line_flags(ctx, buffile).0;
    let command = formatdoc!(
         "evaluate-commands \"set-option buffer lsp_diagnostic_lines {version} {line_flags} '0|%opt[lsp_diagnostic_line_error_sign]'\"
          set-option buffer lsp_inlay_code_lenses {version} {inlay_code_lenses}",
    );
    let command = format!(
        "evaluate-commands -buffer {} %§{}§",
        editor_quote(buffile),
        command.replace('§', "§§")
    );
    ctx.exec(EditorMeta::default(), command);
}

#[derive(Clone, Debug)]
pub struct CodeLensOptions {
    pub selection_desc: String,
}

pub fn resolve_and_perform_code_lens(meta: EditorMeta, params: CodeLensOptions, ctx: &mut Context) {
    let (range, _cursor) = parse_kakoune_range(&params.selection_desc);
    let document = match ctx.documents.get(&meta.buffile) {
        Some(document) => document,
        None => return,
    };

    if let Some((server_id, lens)) = ctx
        .code_lenses
        .get(&meta.buffile)
        .and_then(|lenses| {
            lenses.iter().find(|(server_id, lens)| {
                let ServerSettings {
                    offset_encoding, ..
                } = ctx.server(*server_id);
                let range = kakoune_range_to_lsp(&range, &document.text, *offset_encoding);
                ranges_touch_same_line(lens.range, range)
            })
        })
        .filter(|(_, lens)| lens.command.is_none())
        .cloned()
    {
        let mut req_params = HashMap::new();
        req_params.insert(server_id, vec![lens]);

        ctx.call::<CodeLensResolve, _>(
            meta,
            RequestParams::Each(req_params),
            |ctx: &mut Context, meta, results| perform_code_lens(meta, &results, ctx),
        );
        return;
    }

    let no_lenses = vec![];
    let lenses = ctx.code_lenses.get(&meta.buffile).unwrap_or(&no_lenses);
    let mut lenses = lenses
        .iter()
        .filter(|(server_id, lens)| {
            let ServerSettings {
                offset_encoding, ..
            } = ctx.server(*server_id);
            let range = kakoune_range_to_lsp(&range, &document.text, *offset_encoding);
            ranges_touch_same_line(lens.range, range)
        })
        .map(|(a, b)| (*a, b.clone()))
        .collect::<Vec<_>>();

    lenses.sort_by_key(|(_server_name, lens)| {
        let Range { start, end } = lens.range;
        end.line - start.line
    });

    if lenses.is_empty() {
        ctx.show_error(meta, "no code lens in selection");
        return;
    }

    perform_code_lens(meta, &lenses, ctx);
}

fn perform_code_lens(meta: EditorMeta, lenses: &[(ServerId, CodeLens)], ctx: &Context) {
    let command = format!(
        "lsp-perform-code-lens {}",
        lenses
            .iter()
            .filter(|(_, lens)| lens.command.is_some())
            .map(|(_, lens)| {
                let command = lens.command.as_ref().unwrap();
                format!(
                    "{} {}",
                    &editor_quote(&command.title),
                    &editor_quote(&execute_command_editor_command(command, false)),
                )
            })
            .join(" "),
    );
    ctx.exec(meta, command)
}
