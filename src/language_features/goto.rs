use crate::context::{Context, RequestParams};
use crate::position::*;
use crate::types::{EditorMeta, EditorParams, KakouneRange, PositionParams};
use crate::util::{editor_quote, short_file_path};
use indoc::formatdoc;
use itertools::Itertools;
use lsp_types::request::{
    GotoDeclaration, GotoDefinition, GotoImplementation, GotoTypeDefinition, References,
};
use lsp_types::*;
use serde::Deserialize;
use url::Url;

pub fn goto(meta: EditorMeta, result: Option<GotoDefinitionResponse>, ctx: &mut Context) {
    let locations = match result {
        Some(GotoDefinitionResponse::Scalar(location)) => vec![location],
        Some(GotoDefinitionResponse::Array(locations)) => locations,
        Some(GotoDefinitionResponse::Link(locations)) => locations
            .into_iter()
            .map(
                |LocationLink {
                     target_uri: uri,
                     target_selection_range: range,
                     ..
                 }| Location { uri, range },
            )
            .collect(),
        None => return,
    };
    match locations.len() {
        0 => {}
        1 => {
            goto_location(meta, &locations[0], ctx);
        }
        _ => {
            goto_locations(meta, &locations, ctx);
        }
    }
}

pub fn edit_at_range(buffile: &str, range: KakouneRange) -> String {
    formatdoc!(
        "edit -existing {}
         select {}
         execute-keys <c-s>vv",
        editor_quote(buffile),
        range,
    )
}

fn goto_location(meta: EditorMeta, Location { uri, range }: &Location, ctx: &mut Context) {
    let path = uri.to_file_path().unwrap();
    let path_str = path.to_str().unwrap();
    let (_, server) = ctx.language_servers.first_key_value().unwrap();
    if let Some(contents) = get_file_contents(path_str, ctx) {
        let range = lsp_range_to_kakoune(range, &contents, server.offset_encoding);
        let command = format!(
            "evaluate-commands -try-client %opt{{jumpclient}} -- {}",
            editor_quote(&edit_at_range(path_str, range)),
        );
        ctx.exec(meta, command);
    }
}

fn goto_locations(meta: EditorMeta, locations: &[Location], ctx: &mut Context) {
    let (_, server) = ctx.language_servers.first_key_value().unwrap();
    let select_location = locations
        .iter()
        .group_by(|Location { uri, .. }| uri.to_file_path().unwrap())
        .into_iter()
        .map(|(path, locations)| {
            let path_str = path.to_str().unwrap();
            let contents = match get_file_contents(path_str, ctx) {
                Some(contents) => contents,
                None => return "".into(),
            };
            locations
                .map(|Location { range, .. }| {
                    let pos = lsp_range_to_kakoune(range, &contents, server.offset_encoding).start;
                    if range.start.line as usize >= contents.len_lines() {
                        return "".into();
                    }
                    format!(
                        "{}:{}:{}:{}",
                        short_file_path(path_str, &server.root_path),
                        pos.line,
                        pos.column,
                        contents.line(range.start.line as usize),
                    )
                })
                .join("")
        })
        .join("");
    let command = format!(
        "lsp-show-goto-choices {} {}",
        editor_quote(&server.root_path),
        editor_quote(&select_location),
    );
    ctx.exec(meta, command);
}

pub fn text_document_definition(
    declaration: bool,
    meta: EditorMeta,
    params: EditorParams,
    ctx: &mut Context,
) {
    let params = PositionParams::deserialize(params).unwrap();
    let req_params = GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&meta.buffile).unwrap(),
            },
            position: get_lsp_position(&meta.buffile, &params.position, ctx).unwrap(),
        },
        partial_result_params: Default::default(),
        work_done_progress_params: Default::default(),
    };
    if declaration {
        ctx.call::<GotoDeclaration, _>(
            meta,
            RequestParams::All(vec![req_params]),
            move |ctx: &mut Context, meta, mut result| {
                if let Some((_, result)) = result.pop() {
                    goto(meta, result, ctx);
                }
            },
        );
    } else {
        ctx.call::<GotoDefinition, _>(
            meta,
            RequestParams::All(vec![req_params]),
            move |ctx: &mut Context, meta, mut result| {
                if let Some((_, result)) = result.pop() {
                    goto(meta, result, ctx);
                }
            },
        );
    }
}

pub fn text_document_implementation(meta: EditorMeta, params: EditorParams, ctx: &mut Context) {
    let params = PositionParams::deserialize(params).unwrap();
    let req_params = GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&meta.buffile).unwrap(),
            },
            position: get_lsp_position(&meta.buffile, &params.position, ctx).unwrap(),
        },
        partial_result_params: Default::default(),
        work_done_progress_params: Default::default(),
    };
    ctx.call::<GotoImplementation, _>(
        meta,
        RequestParams::All(vec![req_params]),
        move |ctx: &mut Context, meta, mut result| {
            if let Some((_, result)) = result.pop() {
                goto(meta, result, ctx);
            }
        },
    );
}

pub fn text_document_type_definition(meta: EditorMeta, params: EditorParams, ctx: &mut Context) {
    let params = PositionParams::deserialize(params).unwrap();
    let req_params = GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&meta.buffile).unwrap(),
            },
            position: get_lsp_position(&meta.buffile, &params.position, ctx).unwrap(),
        },
        partial_result_params: Default::default(),
        work_done_progress_params: Default::default(),
    };
    ctx.call::<GotoTypeDefinition, _>(
        meta,
        RequestParams::All(vec![req_params]),
        move |ctx: &mut Context, meta, mut result| {
            if let Some((_, result)) = result.pop() {
                goto(meta, result, ctx);
            }
        },
    );
}

pub fn text_document_references(meta: EditorMeta, params: EditorParams, ctx: &mut Context) {
    let params = PositionParams::deserialize(params).unwrap();
    let req_params = ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&meta.buffile).unwrap(),
            },
            position: get_lsp_position(&meta.buffile, &params.position, ctx).unwrap(),
        },
        context: ReferenceContext {
            include_declaration: true,
        },
        partial_result_params: Default::default(),
        work_done_progress_params: Default::default(),
    };
    ctx.call::<References, _>(
        meta,
        RequestParams::All(vec![req_params]),
        move |ctx: &mut Context, meta, mut result| {
            if let Some((_, result)) = result.pop() {
                goto(meta, result.map(GotoDefinitionResponse::Array), ctx);
            }
        },
    );
}
