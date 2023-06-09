use std::collections::HashMap;

use crate::context::*;
use crate::types::*;
use crate::util::*;
use lsp_types::request::Request;
use lsp_types::*;

pub struct SwitchSourceHeaderRequest {}

impl Request for SwitchSourceHeaderRequest {
    type Params = TextDocumentIdentifier;
    type Result = Option<Url>;
    const METHOD: &'static str = "textDocument/switchSourceHeader";
}

pub fn switch_source_header(meta: EditorMeta, ctx: &mut Context) {
    let (server_name, _) = meta
        .server
        .as_ref()
        .and_then(|name| ctx.language_servers.get_key_value(name))
        .or_else(|| ctx.language_servers.first_key_value())
        .unwrap();
    let mut req_params = HashMap::new();
    req_params.insert(
        server_name.clone(),
        vec![TextDocumentIdentifier {
            uri: Url::from_file_path(&meta.buffile).unwrap(),
        }],
    );

    ctx.call::<SwitchSourceHeaderRequest, _>(
        meta,
        RequestParams::Each(req_params),
        move |ctx, meta, results| {
            let response = match results.into_iter().find(|(_, v)| v.is_some()) {
                Some((_, result)) => result,
                None => None,
            };

            if let Some(response) = response {
                let command = format!(
                    "evaluate-commands -try-client %opt{{jumpclient}} -verbatim -- edit -existing {}",
                    editor_quote(response.to_file_path().unwrap().to_str().unwrap()),
                );
                ctx.exec(meta, command);
            }
        },
    );
}
