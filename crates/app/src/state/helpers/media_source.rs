//! Port of upstream/packages/client-runtime/src/mediaSource.ts

use turin_contracts::ThreadId;

use super::markdown_images::{
    MarkdownImageSource, classify_markdown_image_source, markdown_image_source_fragment,
};
use super::media_reference::{
    MediaReference, media_file_reference, media_reference_file_name, media_url_reference,
};
use super::shared_support::{
    file_basename, media_mime_type, media_mime_type_from_extension, split_file_path_position,
    split_markdown_link_search_and_hash,
};

/// `Extract<AssetResource, { _tag: "media-file" }>`. The `AssetResource` contract is on the
/// cut list (`assets`), so only the one variant this helper builds exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSourceResource {
    pub thread_id: ThreadId,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
        }
    }
}

/// How the bytes of a resolved source can be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaAccess {
    Direct { uri: String },
    Environment { resource: MediaSourceResource },
    Unavailable,
}

/// What a piece of authored media is and how its bytes can be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMediaSource {
    pub kind: MediaKind,
    pub mime_type: String,
    pub name: String,
    pub reference: Option<MediaReference>,
    pub src_fragment: String,
    pub access: MediaAccess,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ResolveMediaSourceInput<'a> {
    pub thread_id: Option<&'a ThreadId>,
    pub workspace_root: Option<&'a str>,
    pub resolved_file_path: Option<&'a str>,
    /// Image syntax can target an endpoint without a recognizable extension.
    pub image_embed: bool,
}

fn classify(source: &str, input: &ResolveMediaSourceInput<'_>) -> MarkdownImageSource {
    match input.resolved_file_path {
        None => classify_markdown_image_source(Some(source), input.workspace_root),
        Some(path) => MarkdownImageSource::WorkspaceFile {
            path: path.to_owned(),
        },
    }
}

pub fn resolve_media_source(
    source: &str,
    input: &ResolveMediaSourceInput<'_>,
) -> Option<ResolvedMediaSource> {
    let classified = classify(source, input);
    if classified == MarkdownImageSource::Blocked {
        return None;
    }

    let path = match &classified {
        MarkdownImageSource::Direct { uri } => split_markdown_link_search_and_hash(uri).path,
        MarkdownImageSource::WorkspaceFile { path } => split_file_path_position(path, "").path,
        MarkdownImageSource::Blocked => return None,
    };
    // Local paths have already been decoded. Do not interpret literal #, ?, or % characters again.
    let basename = file_basename(&path);
    let detected_mime_type = match &classified {
        MarkdownImageSource::Direct { uri } => media_mime_type(uri),
        _ => basename
            .rfind('.')
            .and_then(|index| media_mime_type_from_extension(&basename[index..])),
    };
    let mime_type =
        detected_mime_type.or_else(|| input.image_embed.then(|| "image/*".to_owned()))?;
    let kind = if mime_type.starts_with("video/") {
        MediaKind::Video
    } else {
        MediaKind::Image
    };

    let reference = match &classified {
        MarkdownImageSource::Direct { uri } => media_url_reference(uri).map(MediaReference::Url),
        _ => Some(MediaReference::File(media_file_reference(
            &path,
            input.workspace_root,
        ))),
    };
    let name = reference
        .as_ref()
        .and_then(media_reference_file_name)
        .filter(|name| !name.is_empty())
        .or_else(|| (!basename.is_empty()).then(|| basename.to_owned()))
        .unwrap_or_else(|| kind.as_str().to_owned());
    let src_fragment = markdown_image_source_fragment(source);

    let access = match classified {
        MarkdownImageSource::Direct { uri } => MediaAccess::Direct { uri },
        _ => match input.thread_id {
            None => MediaAccess::Unavailable,
            Some(thread_id) => MediaAccess::Environment {
                resource: MediaSourceResource {
                    thread_id: thread_id.clone(),
                    path,
                },
            },
        },
    };
    Some(ResolvedMediaSource {
        kind,
        mime_type,
        name,
        reference,
        src_fragment,
        access,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::helpers::media_reference::{MediaFileReference, MediaUrlReference};

    fn thread_id() -> ThreadId {
        ThreadId::new("thread-1").unwrap()
    }

    fn input<'a>(thread_id: &'a ThreadId) -> ResolveMediaSourceInput<'a> {
        ResolveMediaSourceInput {
            thread_id: Some(thread_id),
            ..Default::default()
        }
    }

    fn url_reference(url: &str) -> Option<MediaReference> {
        Some(MediaReference::Url(MediaUrlReference {
            url: url.to_owned(),
        }))
    }

    fn file_reference(path: &str, relative_path: Option<&str>) -> Option<MediaReference> {
        Some(MediaReference::File(MediaFileReference {
            path: path.to_owned(),
            relative_path: relative_path.map(str::to_owned),
        }))
    }

    mod direct_urls {
        use super::*;

        #[test]
        fn keeps_the_authored_url_and_decodes_the_display_name_once() {
            let thread_id = thread_id();
            let href = "https://cdn.example.com/clip%20one%2520%2Emp4?signature=a%2fb#t=2";
            assert_eq!(
                resolve_media_source(href, &input(&thread_id)),
                Some(ResolvedMediaSource {
                    kind: MediaKind::Video,
                    mime_type: "video/mp4".to_owned(),
                    name: "clip one%20.mp4".to_owned(),
                    reference: url_reference(href),
                    src_fragment: "#t=2".to_owned(),
                    access: MediaAccess::Direct {
                        uri: href.to_owned()
                    },
                })
            );
        }

        #[test]
        fn keeps_protocol_relative_urls_as_authored_clients_add_the_scheme() {
            let thread_id = thread_id();
            let href = "//cdn.example.com/clip.mp4?sig=1#t=2";
            let resolved = resolve_media_source(href, &input(&thread_id)).unwrap();
            assert_eq!(
                resolved.access,
                MediaAccess::Direct {
                    uri: href.to_owned()
                }
            );
            assert_eq!(resolved.reference, url_reference(href));
        }

        #[test]
        fn accepts_extensionless_image_embeds_only_when_asked() {
            let thread_id = thread_id();
            let href = "https://cdn.example.com/render?id=42";
            assert_eq!(resolve_media_source(href, &input(&thread_id)), None);
            let resolved = resolve_media_source(
                href,
                &ResolveMediaSourceInput {
                    image_embed: true,
                    ..input(&thread_id)
                },
            )
            .unwrap();
            assert_eq!(resolved.kind, MediaKind::Image);
            assert_eq!(resolved.mime_type, "image/*");
            assert_eq!(resolved.name, "render");
        }

        #[test]
        fn loads_data_and_blob_urls_directly() {
            let thread_id = thread_id();
            for href in ["data:image/png;base64,AAAA", "blob:https://app.t3.codes/id"] {
                let resolved = resolve_media_source(
                    href,
                    &ResolveMediaSourceInput {
                        image_embed: true,
                        ..input(&thread_id)
                    },
                )
                .unwrap();
                assert_eq!(
                    resolved.access,
                    MediaAccess::Direct {
                        uri: href.to_owned()
                    },
                    "{href}"
                );
            }
        }
    }

    mod host_paths {
        use super::*;

        // POSIX, Windows, UNC, and file URLs must all reach the same media-file resource.
        #[test]
        fn resolves_host_paths_through_the_environment() {
            let thread_id = thread_id();
            let cases = [
                ("/tmp/frame%23one.png:12", "/tmp/frame#one.png"),
                ("/tmp/frame%3Fone.png:12:3", "/tmp/frame?one.png"),
                ("/tmp/frame%2523one.png:12", "/tmp/frame%23one.png"),
                ("C:\\Users\\demo\\frame.png", "C:\\Users\\demo\\frame.png"),
                ("/C:/Users/demo/frame.png", "C:/Users/demo/frame.png"),
                ("file:///C:/Users/demo/frame.png", "C:/Users/demo/frame.png"),
                (
                    "file://server/share/frame.png",
                    "\\\\server\\share\\frame.png",
                ),
                (
                    "\\\\server\\share\\frame.png",
                    "\\\\server\\share\\frame.png",
                ),
            ];
            for (href, path) in cases {
                assert_eq!(
                    resolve_media_source(
                        href,
                        &ResolveMediaSourceInput {
                            workspace_root: Some("/repo"),
                            ..input(&thread_id)
                        },
                    ),
                    Some(ResolvedMediaSource {
                        kind: MediaKind::Image,
                        mime_type: "image/png".to_owned(),
                        name: path.rsplit(['\\', '/']).next().unwrap().to_owned(),
                        reference: file_reference(path, None),
                        src_fragment: String::new(),
                        access: MediaAccess::Environment {
                            resource: MediaSourceResource {
                                thread_id: thread_id.clone(),
                                path: path.to_owned(),
                            },
                        },
                    }),
                    "{href}"
                );
            }
        }

        #[test]
        fn joins_relative_paths_to_the_workspace_and_records_the_relative_reference() {
            let thread_id = thread_id();
            assert_eq!(
                resolve_media_source(
                    "screens/logo.svg?v=2#mark",
                    &ResolveMediaSourceInput {
                        workspace_root: Some("/repo"),
                        ..input(&thread_id)
                    },
                ),
                Some(ResolvedMediaSource {
                    kind: MediaKind::Image,
                    mime_type: "image/svg+xml".to_owned(),
                    name: "logo.svg".to_owned(),
                    reference: file_reference("/repo/screens/logo.svg", Some("screens/logo.svg")),
                    src_fragment: "#mark".to_owned(),
                    access: MediaAccess::Environment {
                        resource: MediaSourceResource {
                            thread_id: thread_id.clone(),
                            path: "/repo/screens/logo.svg".to_owned(),
                        },
                    },
                })
            );
        }

        #[test]
        fn prefers_a_caller_resolved_path_over_classifying_the_source() {
            let thread_id = thread_id();
            let resolved = resolve_media_source(
                "logo.svg",
                &ResolveMediaSourceInput {
                    workspace_root: Some("/repo"),
                    resolved_file_path: Some("/repo/docs/logo.svg"),
                    ..input(&thread_id)
                },
            )
            .unwrap();
            assert_eq!(
                resolved.access,
                MediaAccess::Environment {
                    resource: MediaSourceResource {
                        thread_id: thread_id.clone(),
                        path: "/repo/docs/logo.svg".to_owned(),
                    },
                }
            );
            assert_eq!(
                resolved.reference,
                file_reference("/repo/docs/logo.svg", Some("docs/logo.svg"))
            );
        }

        #[test]
        fn separates_a_playback_fragment_from_literal_filename_characters() {
            let thread_id = thread_id();
            let resolved =
                resolve_media_source("/tmp/clip%23one.mp4#t=2", &input(&thread_id)).unwrap();
            assert_eq!(resolved.kind, MediaKind::Video);
            assert_eq!(resolved.src_fragment, "#t=2");
            assert_eq!(
                resolved.access,
                MediaAccess::Environment {
                    resource: MediaSourceResource {
                        thread_id: thread_id.clone(),
                        path: "/tmp/clip#one.mp4".to_owned(),
                    },
                }
            );
        }

        #[test]
        fn cannot_be_loaded_without_a_thread_to_mint_through() {
            let resolved = resolve_media_source(
                "/tmp/clip.mp4",
                &ResolveMediaSourceInput {
                    thread_id: None,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(resolved.kind, MediaKind::Video);
            assert_eq!(resolved.access, MediaAccess::Unavailable);
        }

        #[test]
        fn returns_none_for_the_non_media_path() {
            let thread_id = thread_id();
            for href in ["notes.txt", "/repo/README", "/repo/archive.zip"] {
                assert_eq!(
                    resolve_media_source(
                        href,
                        &ResolveMediaSourceInput {
                            workspace_root: Some("/repo"),
                            ..input(&thread_id)
                        },
                    ),
                    None,
                    "{href}"
                );
            }
        }
    }

    #[test]
    fn serves_t3_attachment_files_in_place_like_any_other_host_path() {
        let thread_id = thread_id();
        let attachment_id =
            "11111111-1111-4111-8111-111111111111-22222222-2222-4222-8222-222222222222-mp4";
        let path = format!("/home/demo/.t3/userdata/attachments/{attachment_id}.mp4");
        let resolved = resolve_media_source(
            &path,
            &ResolveMediaSourceInput {
                workspace_root: Some("/repo"),
                ..input(&thread_id)
            },
        )
        .unwrap();
        assert_eq!(resolved.kind, MediaKind::Video);
        assert_eq!(
            resolved.access,
            MediaAccess::Environment {
                resource: MediaSourceResource {
                    thread_id: thread_id.clone(),
                    path
                },
            }
        );
    }
}
