use crate::context::{Context, RequestParams};
use crate::position::*;
use crate::types::{EditorMeta, EditorParams, KakouneRange, PositionParams, ServerName};
use crate::util::{editor_quote, short_file_path};
use indoc::formatdoc;
use itertools::Itertools;
use lsp_types::request::{
    GotoDeclaration, GotoDefinition, GotoImplementation, GotoTypeDefinition, References,
};
use lsp_types::*;
use serde::Deserialize;
use url::Url;

pub fn goto(
    meta: EditorMeta,
    result: (ServerName, Option<GotoDefinitionResponse>),
    ctx: &mut Context,
) {
    let (server_name, result) = result;
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
            goto_location(&server_name, meta, &locations[0], ctx);
        }
        _ => {
            goto_locations(&server_name, meta, &locations, ctx);
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

fn goto_location(
    server_name: &ServerName,
    meta: EditorMeta,
    Location { uri, range }: &Location,
    ctx: &mut Context,
) {
    let path = uri.to_file_path().unwrap();
    let path_str = path.to_str().unwrap();
    if let Some(contents) = get_file_contents(path_str, ctx) {
        let server = &ctx.language_servers[server_name];
        let range = lsp_range_to_kakoune(range, &contents, server.offset_encoding);
        let command = format!(
            "evaluate-commands -try-client %opt{{jumpclient}} -- {}",
            editor_quote(&edit_at_range(path_str, range)),
        );
        ctx.exec(meta, command);
    }
}

fn goto_locations(
    server_name: &ServerName,
    meta: EditorMeta,
    locations: &[Location],
    ctx: &mut Context,
) {
    let server = &ctx.language_servers[server_name];
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
    let req_params = ctx
        .language_servers
        .iter()
        .map(|(server_name, server_settings)| {
            (
                server_name.clone(),
                vec![GotoDefinitionParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier {
                            uri: Url::from_file_path(&meta.buffile).unwrap(),
                        },
                        position: get_lsp_position(
                            server_settings,
                            &meta.buffile,
                            &params.position,
                            ctx,
                        )
                        .unwrap(),
                    },
                    partial_result_params: Default::default(),
                    work_done_progress_params: Default::default(),
                }],
            )
        })
        .collect();
    let req_params = RequestParams::Each(req_params);
    if declaration {
        ctx.call::<GotoDeclaration, _>(
            meta,
            req_params,
            move |ctx: &mut Context, meta, results| {
                let result = match results.into_iter().find(|(_, v)| v.is_some()) {
                    Some(result) => result,
                    None => {
                        let entry = ctx.language_servers.first_entry().unwrap();
                        (entry.key().clone(), None)
                    }
                };

                goto(meta, result, ctx);
            },
        );
    } else {
        ctx.call::<GotoDefinition, _>(meta, req_params, move |ctx: &mut Context, meta, results| {
            let result = match results.into_iter().find(|(_, v)| v.is_some()) {
                Some(result) => result,
                None => {
                    let entry = ctx.language_servers.first_entry().unwrap();
                    (entry.key().clone(), None)
                }
            };

            goto(meta, result, ctx);
        });
    }
}

pub fn text_document_implementation(meta: EditorMeta, params: EditorParams, ctx: &mut Context) {
    let params = PositionParams::deserialize(params).unwrap();
    let req_params = ctx
        .language_servers
        .iter()
        .map(|(server_name, server_settings)| {
            (
                server_name.clone(),
                vec![GotoDefinitionParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier {
                            uri: Url::from_file_path(&meta.buffile).unwrap(),
                        },
                        position: get_lsp_position(
                            server_settings,
                            &meta.buffile,
                            &params.position,
                            ctx,
                        )
                        .unwrap(),
                    },
                    partial_result_params: Default::default(),
                    work_done_progress_params: Default::default(),
                }],
            )
        })
        .collect();
    ctx.call::<GotoImplementation, _>(
        meta,
        RequestParams::Each(req_params),
        move |ctx: &mut Context, meta, results| {
            let result = match results.into_iter().find(|(_, v)| v.is_some()) {
                Some(result) => result,
                None => {
                    let entry = ctx.language_servers.first_entry().unwrap();
                    (entry.key().clone(), None)
                }
            };

            goto(meta, result, ctx);
        },
    );
}

pub fn text_document_type_definition(meta: EditorMeta, params: EditorParams, ctx: &mut Context) {
    let params = PositionParams::deserialize(params).unwrap();
    let req_params = ctx
        .language_servers
        .iter()
        .map(|(server_name, server_settings)| {
            (
                server_name.clone(),
                vec![GotoDefinitionParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier {
                            uri: Url::from_file_path(&meta.buffile).unwrap(),
                        },
                        position: get_lsp_position(
                            server_settings,
                            &meta.buffile,
                            &params.position,
                            ctx,
                        )
                        .unwrap(),
                    },
                    partial_result_params: Default::default(),
                    work_done_progress_params: Default::default(),
                }],
            )
        })
        .collect();
    ctx.call::<GotoTypeDefinition, _>(
        meta,
        RequestParams::Each(req_params),
        move |ctx: &mut Context, meta, results| {
            let result = match results.into_iter().find(|(_, v)| v.is_some()) {
                Some(result) => result,
                None => {
                    let entry = ctx.language_servers.first_entry().unwrap();
                    (entry.key().clone(), None)
                }
            };

            goto(meta, result, ctx);
        },
    );
}

pub fn text_document_references(meta: EditorMeta, params: EditorParams, ctx: &mut Context) {
    let params = PositionParams::deserialize(params).unwrap();
    let req_params = ctx
        .language_servers
        .iter()
        .map(|(server_name, server_settings)| {
            (
                server_name.clone(),
                vec![ReferenceParams {
                    text_document_position: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier {
                            uri: Url::from_file_path(&meta.buffile).unwrap(),
                        },
                        position: get_lsp_position(
                            server_settings,
                            &meta.buffile,
                            &params.position,
                            ctx,
                        )
                        .unwrap(),
                    },
                    context: ReferenceContext {
                        include_declaration: true,
                    },
                    partial_result_params: Default::default(),
                    work_done_progress_params: Default::default(),
                }],
            )
        })
        .collect();
    ctx.call::<References, _>(
        meta,
        RequestParams::Each(req_params),
        move |ctx: &mut Context, meta, results| {
            let result = match results.into_iter().find(|(_, v)| v.is_some()) {
                Some(result) => result,
                None => {
                    let entry = ctx.language_servers.first_entry().unwrap();
                    (entry.key().clone(), None)
                }
            };

            let (server_name, loc) = result;
            let loc = loc.map(GotoDefinitionResponse::Array);
            goto(meta, (server_name, loc), ctx);
        },
    );
}
