//! Port of the `createAssetEnvironmentAtoms` environment-choice cases of
//! upstream/packages/client-runtime/src/state/assets.test.ts, and the URL
//! state rules of `assets.ts`.
//!
//! Not ported: the asset collection key cases (`parseAssetCollectionKey`
//! decodes the cut `assets` contract), the query-key identity cases (atom
//! families), the `primary reconnecting` refresh and the project favicon URL
//! cache (needs `projectFaviconCache`, not ported).

use parking_lot::Mutex;

use super::*;

struct FakeRequests {
    remote_id: String,
    remote_failure: Option<AssetUrlError>,
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl AssetUrlRequests for FakeRequests {
    async fn create_url(
        &self,
        environment_id: &str,
        _resource: &AssetResource,
    ) -> Result<AssetCreateUrlResult, AssetUrlError> {
        self.calls.lock().push(environment_id.to_owned());
        if environment_id == self.remote_id
            && let Some(error) = &self.remote_failure
        {
            return Err(error.clone());
        }
        Ok(AssetCreateUrlResult {
            relative_url: format!("/api/assets/{environment_id}/media"),
            expires_at: 999_999.0,
            source_path: None,
            image_dimensions: None,
        })
    }
}

fn error(tag: &str) -> AssetUrlError {
    AssetUrlError {
        tag: tag.to_owned(),
        message: tag.to_owned(),
    }
}

fn media(path: &str) -> AssetResource {
    AssetResource::MediaFile {
        thread_id: "foreign-thread".into(),
        path: path.into(),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Primary {
    Local,
    None,
    Same,
}

struct Scenario {
    name: &'static str,
    path: &'static str,
    error: Option<&'static str>,
    success: bool,
    primary: Primary,
    /// Expected: a fallback to the local environment.
    fallback: bool,
}

fn scenario(name: &'static str, path: &'static str) -> Scenario {
    Scenario {
        name,
        path,
        error: Some("AssetWorkspaceAssetNotFoundError"),
        success: false,
        primary: Primary::Local,
        fallback: false,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn uses_the_correct_environment() {
    let scenarios = [
        Scenario {
            fallback: true,
            ..scenario("missing video", "/tmp/clip.mp4")
        },
        Scenario {
            fallback: true,
            ..scenario("literal filename characters", "/tmp/frame#one?two.png")
        },
        Scenario {
            fallback: true,
            ..scenario("windows path", "C:\\Users\\demo\\clip.mp4")
        },
        Scenario {
            error: Some("AssetWorkspaceAssetInspectionError"),
            fallback: true,
            ..scenario("inspection failure", "/tmp/clip.mp4")
        },
        Scenario {
            error: Some("AssetWorkspaceContextNotFoundError"),
            fallback: true,
            ..scenario("foreign thread", "/tmp/clip.mp4")
        },
        Scenario {
            success: true,
            ..scenario("remote success", "/tmp/clip.mp4")
        },
        scenario("relative path", "clip.mp4"),
        Scenario {
            primary: Primary::None,
            ..scenario("no primary", "/tmp/clip.mp4")
        },
        Scenario {
            primary: Primary::None,
            ..scenario("primary reconnecting", "/tmp/frame.png")
        },
        Scenario {
            primary: Primary::Same,
            ..scenario("same environment", "/tmp/clip.mp4")
        },
        scenario("non-media", "/tmp/report.html"),
        Scenario {
            error: Some("EnvironmentAuthorizationError"),
            ..scenario("authorization failure", "/tmp/clip.mp4")
        },
    ];
    for case in scenarios {
        let remote = "remote";
        let requests = FakeRequests {
            remote_id: remote.to_owned(),
            remote_failure: if case.success {
                None
            } else {
                case.error.map(error)
            },
            calls: Mutex::new(Vec::new()),
        };
        let local = match case.primary {
            Primary::Local => Some(LocalMediaEnvironment {
                environment_id: "local".into(),
                http_base_url: "https://local.test".into(),
            }),
            Primary::Same => Some(LocalMediaEnvironment {
                environment_id: remote.into(),
                http_base_url: "https://local.test".into(),
            }),
            Primary::None => None,
        };
        let result = create_asset_url(&requests, remote, &media(case.path), local.as_ref()).await;
        if case.fallback || case.success {
            let expected = if case.fallback {
                "https://local.test/api/assets/local/media"
            } else {
                "/api/assets/remote/media"
            };
            assert_eq!(result.unwrap().relative_url, expected, "{}", case.name);
        } else {
            assert_eq!(
                result.unwrap_err(),
                error(case.error.unwrap()),
                "{}",
                case.name
            );
        }
        let calls = requests.calls.lock().clone();
        let expected_calls: &[&str] = if case.fallback {
            &["remote", "local"]
        } else {
            &["remote"]
        };
        assert_eq!(calls, expected_calls, "{}", case.name);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn only_media_file_resources_fall_back() {
    let requests = FakeRequests {
        remote_id: "remote".into(),
        remote_failure: Some(error("AssetWorkspaceAssetNotFoundError")),
        calls: Mutex::new(Vec::new()),
    };
    let local = LocalMediaEnvironment {
        environment_id: "local".into(),
        http_base_url: "https://local.test".into(),
    };
    let resource = AssetResource::WorkspaceFile {
        thread_id: "t".into(),
        path: "/tmp/clip.mp4".into(),
    };
    assert!(
        create_asset_url(&requests, "remote", &resource, Some(&local))
            .await
            .is_err()
    );
    assert_eq!(*requests.calls.lock(), ["remote"]);
}

fn result(relative_url: &str) -> AssetCreateUrlResult {
    AssetCreateUrlResult {
        relative_url: relative_url.into(),
        expires_at: 4_000_000_000_000.0,
        source_path: Some("/host/a.png".into()),
        image_dimensions: Some(AssetImageDimensions {
            width: 3,
            height: 2,
        }),
    }
}

#[test]
fn a_settled_result_resolves_against_the_connection() {
    assert_eq!(
        asset_url_state_from_result(
            &AssetUrlResult::Success(result("/api/assets/new/v2-icon.png")),
            Some("https://remote.test")
        ),
        AssetUrlState::Success {
            url: "https://remote.test/api/assets/new/v2-icon.png".into(),
            expires_at: 4_000_000_000_000.0,
            source_path: Some("/host/a.png".into()),
            image_dimensions: Some(AssetImageDimensions {
                width: 3,
                height: 2
            }),
        }
    );
}

#[test]
fn pending_results_and_missing_connections_load_and_failures_fail() {
    let ok = AssetUrlResult::Success(result("/a"));
    assert_eq!(
        asset_url_state_from_result(&AssetUrlResult::Initial, Some("https://remote.test")),
        AssetUrlState::Loading
    );
    assert_eq!(
        asset_url_state_from_result(&ok, None),
        AssetUrlState::Loading
    );
    assert_eq!(
        asset_url_state_from_result(&AssetUrlResult::Failure, None),
        AssetUrlState::Failure {
            missing_file_access: false
        }
    );
    // A relative URL that cannot resolve against a bad base is a failure.
    assert_eq!(
        asset_url_state_from_result(&ok, Some("not a url")),
        AssetUrlState::Failure {
            missing_file_access: false
        }
    );
}

#[test]
fn a_session_without_file_access_blocks_host_files_only() {
    let denied = |is_pending, error| FileAccess {
        can_read_files: false,
        is_pending,
        error,
    };
    let workspace = AssetResource::WorkspaceFile {
        thread_id: "t".into(),
        path: "a".into(),
    };
    let draft = AssetResource::DraftWorkspaceFile {
        cwd: "/r".into(),
        path: "a".into(),
    };
    let favicon = AssetResource::Other {
        tag: "project-favicon".into(),
    };
    assert_eq!(blocked_asset_url_state(None, denied(false, None)), None);
    assert_eq!(
        blocked_asset_url_state(Some(&favicon), denied(false, None)),
        None
    );
    assert_eq!(
        blocked_asset_url_state(
            Some(&media("/a.mp4")),
            FileAccess {
                can_read_files: true,
                is_pending: false,
                error: None
            }
        ),
        None
    );
    for resource in [&workspace, &draft, &media("/a.mp4")] {
        assert_eq!(
            blocked_asset_url_state(Some(resource), denied(true, None)),
            Some(AssetUrlState::Loading)
        );
        assert_eq!(
            blocked_asset_url_state(Some(resource), denied(false, None)),
            Some(AssetUrlState::Failure {
                missing_file_access: true
            })
        );
        assert_eq!(
            blocked_asset_url_state(Some(resource), denied(false, Some("offline"))),
            Some(AssetUrlState::Failure {
                missing_file_access: false
            })
        );
    }
    assert_eq!(
        MISSING_FILE_ACCESS_MESSAGE,
        "This connection cannot read host files. Pair again with a new link to view it."
    );
}
