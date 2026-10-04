use std::{net::Ipv4Addr, time::Duration};

use datalith_core::{Datalith, DatalithService, ServiceConfig, Task, TaskStatus};
use rocket::{
    http::{ContentType, Header, Status},
    local::asynchronous::Client,
};
use serde_json::{Value, json};
use tempfile::TempDir;

async fn client() -> (Client, DatalithService, TempDir) {
    let directory = TempDir::new().unwrap();
    let datalith = Datalith::new(directory.path()).await.unwrap();
    let service = DatalithService::new(datalith, ServiceConfig::default()).await.unwrap();
    let rocket =
        super::create(Ipv4Addr::LOCALHOST.into(), 1111, 1024 * 1024).manage(service.clone());
    (Client::tracked(rocket).await.unwrap(), service, directory)
}

fn multipart(bytes: &[u8], options: Option<Value>) -> Vec<u8> {
    let mut body = Vec::new();
    if let Some(options) = options {
        body.extend_from_slice(
            format!(
                "--test-boundary\r\nContent-Disposition: form-data; \
                 name=\"options\"\r\nContent-Type: application/json\r\n\r\n{options}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(b"--test-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"hello.txt\"\r\nContent-Type: text/plain\r\n\r\n");
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n--test-boundary--\r\n");
    body
}

fn multipart_header() -> Header<'static> {
    Header::new("Content-Type", "multipart/form-data; boundary=test-boundary")
}

async fn wait_task(client: &Client, submitted: Task) -> Task {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let response = client.get(format!("/api/v1/tasks/{}", submitted.id)).dispatch().await;
            assert_eq!(Status::Ok, response.status());
            let task: Task = response.into_json().await.unwrap();
            if task.status.is_terminal() {
                assert_eq!(TaskStatus::Succeeded, task.status, "{:?}", task.error);
                return task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

async fn upload(client: &Client, options: Value) -> Task {
    let response = client
        .post("/api/v1/uploads")
        .header(multipart_header())
        .body(multipart(b"Hello world!", Some(options)))
        .dispatch()
        .await;
    assert_eq!(Status::Accepted, response.status());
    wait_task(client, response.into_json().await.unwrap()).await
}

async fn media_id(client: &Client) -> String {
    let response = client.get("/api/v1/media").dispatch().await;
    let page: Value = response.into_json().await.unwrap();
    assert_eq!("1", page["total"]);
    page["items"][0]["id"].as_str().unwrap().to_owned()
}

#[rocket::async_test]
async fn upload_download_ranges_and_conditional_requests() {
    let (client, service, _directory) = client().await;
    upload(&client, json!({})).await;
    let id = media_id(&client).await;
    let path = format!("/api/v1/media/{id}/content");
    let response = client.get(&path).dispatch().await;
    assert_eq!(Status::Ok, response.status());
    assert_eq!(Some("bytes"), response.headers().get_one("Accept-Ranges"));
    assert!(response.headers().get_one("Last-Modified").is_some());
    assert!(response.headers().get_one("Date").unwrap().ends_with(" GMT"));
    let etag = response.headers().get_one("ETag").unwrap().to_owned();
    assert_eq!("Hello world!", response.into_string().await.unwrap());

    let response = client.head(&path).dispatch().await;
    assert_eq!(Status::Ok, response.status());
    assert_eq!(Some(12), response.body().preset_size());
    assert_eq!("", response.into_string().await.unwrap_or_default());

    let response = client
        .get(&path)
        .header(Header::new("Range", "bytes=1-4"))
        .header(Header::new("If-Range", etag.clone()))
        .dispatch()
        .await;
    assert_eq!(Status::PartialContent, response.status());
    assert_eq!(Some("bytes 1-4/12"), response.headers().get_one("Content-Range"));
    assert_eq!("ello", response.into_string().await.unwrap());

    let response = client.get(&path).header(Header::new("Range", "bytes=-6")).dispatch().await;
    assert_eq!(Status::PartialContent, response.status());
    assert_eq!("world!", response.into_string().await.unwrap());

    let response = client.get(&path).header(Header::new("Range", "bytes=100-200")).dispatch().await;
    assert_eq!(Status::RangeNotSatisfiable, response.status());
    assert_eq!(Some("bytes */12"), response.headers().get_one("Content-Range"));
    drop(response);

    for range in ["bytes=0-1,3-4", "items=1-2"] {
        let response = client.get(&path).header(Header::new("Range", range)).dispatch().await;
        assert_eq!(Status::Ok, response.status());
        assert_eq!("Hello world!", response.into_string().await.unwrap());
    }

    let response = client
        .get(&path)
        .header(Header::new("If-None-Match", format!("W/{etag}")))
        .dispatch()
        .await;
    assert_eq!(Status::NotModified, response.status());
    drop(response);

    let response = client
        .get(&path)
        .header(Header::new("If-Range", "\"different\""))
        .header(Header::new("Range", "bytes=1-4"))
        .dispatch()
        .await;
    assert_eq!(Status::Ok, response.status());
    assert_eq!("Hello world!", response.into_string().await.unwrap());

    let response = client.delete(format!("/api/v1/media/{id}")).dispatch().await;
    assert_eq!(Status::NoContent, response.status());
    drop(response);
    let response = client.get(&path).header(Header::new("If-None-Match", etag)).dispatch().await;
    assert_eq!(Status::NotFound, response.status());
    let request_id = response.headers().get_one("X-Request-Id").unwrap().to_owned();
    let error: Value = response.into_json().await.unwrap();
    assert_eq!("not_found", error["error"]["code"]);
    assert_eq!(request_id, error["request_id"]);
    service.close().await.unwrap();
}

#[rocket::async_test]
async fn docs_serve_the_generated_api_and_embedded_swagger_ui() {
    let (client, service, _directory) = client().await;
    let response = client.get("/api/v1/docs").dispatch().await;
    assert_eq!(Status::PermanentRedirect, response.status());
    assert_eq!(Some("docs/"), response.headers().get_one("Location"));
    drop(response);

    let response = client.get("/api/v1/docs/").dispatch().await;
    assert_eq!(Status::Ok, response.status());
    assert!(response.into_string().await.unwrap().contains("Swagger UI"));
    let response = client.get("/api/v1/docs/swagger-ui-bundle.js").dispatch().await;
    assert_eq!(Status::Ok, response.status());
    assert!(response.into_string().await.unwrap().contains("SwaggerUIBundle"));
    let response = client.get("/api/v1/docs/swagger-initializer.js").dispatch().await;
    assert_eq!(Status::Ok, response.status());
    let initializer = response.into_string().await.unwrap();
    assert!(initializer.contains("json"));
    assert!(initializer.contains("validatorUrl"));

    let response = client.get("/api/v1/docs/json").dispatch().await;
    assert_eq!(Status::Ok, response.status());
    let document: Value = response.into_json().await.unwrap();
    assert_eq!("3.1.0", document["openapi"]);
    assert_eq!("..", document["servers"][0]["url"]);
    assert_eq!("getOpenApi", document["paths"]["/docs/json"]["get"]["operationId"]);
    assert!(document["paths"]["/media/{id}/hls/{track}/init.mp4"]["get"].is_object());
    assert!(
        document["paths"]["/media/{id}/hls/{track}/segment-{sequence}.m4s"]["head"].is_object()
    );
    assert_eq!(
        "application/json",
        document["paths"]["/uploads"]["post"]["requestBody"]["content"]["multipart/form-data"]
            ["encoding"]["options"]["contentType"]
    );

    fn check_references(value: &Value, document: &Value) {
        match value {
            Value::Object(object) => {
                if let Some(Value::String(reference)) = object.get("$ref") {
                    assert!(
                        document.pointer(reference.strip_prefix('#').unwrap()).is_some(),
                        "Missing schema: {reference}"
                    );
                }
                for value in object.values() {
                    check_references(value, document);
                }
            },
            Value::Array(values) => {
                for value in values {
                    check_references(value, document);
                }
            },
            _ => (),
        }
    }
    check_references(&document, &document);
    for path in document["paths"].as_object().unwrap().values() {
        if let Some(responses) = path["head"]["responses"].as_object() {
            for response in responses.values() {
                assert!(response.get("content").is_none());
            }
        }
    }
    let response = client.get("/api/v1/capabilities").dispatch().await;
    let capabilities: Value = response.into_json().await.unwrap();
    serde_json::from_value::<super::openapi::Capabilities>(capabilities).unwrap();
    let submitted = upload(&client, json!({})).await;
    let completed = wait_task(&client, submitted).await;
    serde_json::from_value::<super::openapi::TaskResult>(completed.result.unwrap()).unwrap();

    let response = client.get("/api/v1/openapi.json").dispatch().await;
    assert_eq!(Status::PermanentRedirect, response.status());
    assert_eq!(Some("docs/json"), response.headers().get_one("Location"));
    drop(response);
    service.close().await.unwrap();
}

#[rocket::async_test]
async fn head_does_not_consume_single_use_content() {
    let (client, service, _directory) = client().await;
    upload(&client, json!({"retention": {"single_use": true, "expires_in_seconds": 60}})).await;
    let id = media_id(&client).await;
    let path = format!("/api/v1/media/{id}/content");
    let response = client.head(&path).header(Header::new("If-None-Match", "*")).dispatch().await;
    assert_eq!(Status::Ok, response.status());
    assert_eq!(None, response.headers().get_one("ETag"));
    drop(response);

    let response = client.get(format!("/api/v1/media/{id}")).dispatch().await;
    assert_eq!(Status::Ok, response.status());
    drop(response);
    let response = client
        .get(&path)
        .header(Header::new("Range", "bytes=0-1"))
        .header(Header::new("If-None-Match", "*"))
        .dispatch()
        .await;
    assert_eq!(Status::Ok, response.status());
    assert_eq!(Some("no-store"), response.headers().get_one("Cache-Control"));
    assert_eq!(None, response.headers().get_one("ETag"));
    assert_eq!("Hello world!", response.into_string().await.unwrap());
    let response = client.get(&path).dispatch().await;
    assert_eq!(Status::NotFound, response.status());
    drop(response);
    service.close().await.unwrap();
}

#[rocket::async_test]
async fn idempotent_upload_and_archive_round_trip() {
    let (source, source_service, _source_directory) = client().await;
    let body = multipart(b"Hello world!", Some(json!({})));
    let mut task_id = None;
    for _ in 0..2 {
        let response = source
            .post("/api/v1/uploads")
            .header(multipart_header())
            .header(Header::new("Idempotency-Key", "upload-test"))
            .body(body.clone())
            .dispatch()
            .await;
        assert_eq!(Status::Accepted, response.status());
        let task = wait_task(&source, response.into_json().await.unwrap()).await;
        if let Some(id) = task_id {
            assert_eq!(id, task.id);
        }
        task_id = Some(task.id);
    }
    let id = media_id(&source).await;
    let response =
        source.post("/api/v1/exports").header(ContentType::JSON).body("{}").dispatch().await;
    assert_eq!(Status::Accepted, response.status());
    let task = wait_task(&source, response.into_json().await.unwrap()).await;
    let response = source.get(format!("/api/v1/tasks/{}/artifact", task.id)).dispatch().await;
    assert_eq!(Status::Ok, response.status());
    let archive = response.into_bytes().await.unwrap();

    let (target, target_service, _target_directory) = client().await;
    let response = target
        .post("/api/v1/imports")
        .header(multipart_header())
        .body(multipart(&archive, None))
        .dispatch()
        .await;
    assert_eq!(Status::Accepted, response.status());
    wait_task(&target, response.into_json().await.unwrap()).await;
    assert_eq!(id, media_id(&target).await);
    let response = target.get(format!("/api/v1/media/{id}/content")).dispatch().await;
    assert_eq!(Status::Ok, response.status());
    assert_eq!("Hello world!", response.into_string().await.unwrap());
    source_service.close().await.unwrap();
    target_service.close().await.unwrap();
}
