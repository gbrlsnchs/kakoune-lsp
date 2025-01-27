use super::code_action::execute_command_editor_command;
use crate::capabilities::CAPABILITY_CODE_LENS;
use crate::capabilities::CAPABILITY_EXECUTE_COMMANDS;
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
use serde::Deserialize;

pub fn text_document_code_lens(meta: EditorMeta, ctx: &mut Context) {
    if !server_has_capability(ctx, CAPABILITY_CODE_LENS)
        || !server_has_capability(ctx, CAPABILITY_EXECUTE_COMMANDS)
    {
        return;
    }

    let req_params = CodeLensParams {
        text_document: TextDocumentIdentifier {
            uri: Url::from_file_path(&meta.buffile).unwrap(),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    ctx.call::<CodeLensRequest, _>(meta, req_params, |ctx: &mut Context, meta, result| {
        editor_code_lens(meta, result, ctx)
    });
}

fn editor_code_lens(meta: EditorMeta, result: Option<Vec<CodeLens>>, ctx: &mut Context) {
    let mut lenses = result.unwrap_or_default();
    lenses.sort_by_key(|lens| lens.range.start);

    let buffile = &meta.buffile;
    let document = match ctx.documents.get(buffile) {
        Some(document) => document,
        None => {
            ctx.code_lenses.remove(buffile);
            return;
        }
    };
    let version = document.version;
    let range_specs = lenses
        .iter()
        .map(|lens| {
            let label = lens.command.as_ref().map_or("", |v| &v.title);
            let position =
                lsp_position_to_kakoune(&lens.range.start, &document.text, ctx.offset_encoding);
            let line = position.line;
            let column = position.column;
            lazy_static! {
                static ref CODE_LENS_INDICATOR: &'static str =
                    wcwidth::expected_width_or_fallback("🔎", 2, "[L]");
            }

            editor_quote(&format!(
                "{line}.{column}+0|{{InlayCodeLens}}[{} {}] ",
                *CODE_LENS_INDICATOR,
                escape_tuple_element(&escape_kakoune_markup(label))
            ))
        })
        .join(" ");

    ctx.code_lenses.insert(meta.buffile.clone(), lenses);

    let line_flags = gather_line_flags(ctx, buffile).0;
    let command = formatdoc!(
         "evaluate-commands \"set-option buffer lsp_diagnostic_lines {version} {line_flags} '0|%opt[lsp_diagnostic_line_error_sign]'\"
          set-option buffer lsp_inlay_code_lenses {version} {range_specs}",
    );
    let command = format!(
        "evaluate-commands -buffer {} %§{}§",
        editor_quote(buffile),
        command.replace('§', "§§")
    );
    let meta = ctx.meta_for_buffer_version(None, buffile, version);
    ctx.exec(meta, command);
}

#[derive(Clone, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CodeLensOptions {
    pub selection_desc: String,
}

pub fn resolve_and_perform_code_lens(meta: EditorMeta, params: EditorParams, ctx: &mut Context) {
    let params = CodeLensOptions::deserialize(params)
        .expect("Params should follow CodeLensParams structure");
    let (range, _cursor) = parse_kakoune_range(&params.selection_desc);
    let document = match ctx.documents.get(&meta.buffile) {
        Some(document) => document,
        None => return,
    };
    let range = kakoune_range_to_lsp(&range, &document.text, ctx.offset_encoding);

    if let Some(lens) = ctx
        .code_lenses
        .get(&meta.buffile)
        .and_then(|lenses| {
            lenses
                .iter()
                .find(|lens| ranges_touch_same_line(lens.range, range))
        })
        .filter(|lens| lens.command.is_none())
        .cloned()
    {
        ctx.call::<CodeLensResolve, _>(meta, lens, |ctx: &mut Context, meta, lens| {
            perform_code_lens(meta, &[&lens], ctx)
        });
        return;
    }

    let lenses = match ctx.code_lenses.get(&meta.buffile) {
        Some(lenses) => lenses,
        None => return,
    };
    let lenses = lenses
        .iter()
        .filter(|lens| ranges_touch_same_line(lens.range, range))
        .collect::<Vec<_>>();

    if lenses.is_empty() {
        ctx.exec(meta, "lsp-show-error 'no code lens in selection'");
        return;
    }

    perform_code_lens(meta, &lenses, ctx);
}

fn perform_code_lens(meta: EditorMeta, lenses: &[&CodeLens], ctx: &Context) {
    let command = format!(
        "lsp-perform-code-lens {}",
        lenses
            .iter()
            .filter(|lens| lens.command.is_some())
            .map(|lens| {
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
