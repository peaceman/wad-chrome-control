use crate::config::AppConfig;

use anyhow::Context;
use std::net::SocketAddr;
use tracing::info;

pub fn start_web_server(config: AppConfig) -> anyhow::Result<(u16, impl warp::Future)> {
    let port = portpicker::pick_unused_port()
        .with_context(|| "Failed to allocate unused port to use as webserver port")?;

    let handlebars = templates::load_handlebars()?;
    let routes = filters::create_routes(&config, handlebars);
    let socket = SocketAddr::new(config.http.listen_ip, port);
    let fut = warp::serve(routes).run(socket);

    info!("Started web server on {}", socket);

    Ok((port, fut))
}

mod templates {
    use std::sync::Arc;

    use handlebars::Handlebars;
    use serde::Serialize;

    pub static CLIENT_ID: &str = "client-id";
    pub static MISSING_CLIENT_ID: &str = "missing-client-id";

    static CLIENT_ID_TEMPLATE: &str = include_str!("templates/client-id.hbs");
    static MISSING_CLIENT_ID_TEMPLATE: &str = include_str!("templates/missing-client-id.hbs");

    #[derive(Serialize)]
    pub struct ClientIdData {
        pub client_id: String,
    }

    pub fn load_handlebars() -> anyhow::Result<Arc<Handlebars<'static>>> {
        let mut hb = Handlebars::new();
        hb.register_template_string(CLIENT_ID, CLIENT_ID_TEMPLATE)?;
        hb.register_template_string(MISSING_CLIENT_ID, MISSING_CLIENT_ID_TEMPLATE)?;

        Ok(Arc::new(hb))
    }
}

mod filters {
    use super::handlers;
    use crate::config::AppConfig;

    use std::convert::Infallible;
    use std::path::PathBuf;
    use std::sync::Arc;

    use handlebars::Handlebars;
    use warp::Filter;

    pub fn create_routes(
        config: &AppConfig,
        handlebars: Arc<Handlebars<'static>>,
    ) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
        get_client_id(config, handlebars).or(get_healthz())
    }

    /// GET /
    fn get_client_id(
        config: &AppConfig,
        hb: Arc<Handlebars<'static>>,
    ) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
        warp::path::end()
            .and(client_id_path(Arc::clone(config)))
            .and(handlebars(Arc::clone(&hb)))
            .and_then(handlers::get_client_id)
    }

    /// GET /healthz
    fn get_healthz() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
        warp::path("healthz").map(|| "🧩")
    }

    fn client_id_path(
        config: AppConfig,
    ) -> impl Filter<Extract = (PathBuf,), Error = Infallible> + Clone {
        warp::any().map(move || config.data_base_folder.join("client-id"))
    }

    fn handlebars(
        handlebars: Arc<Handlebars>,
    ) -> impl Filter<Extract = (Arc<Handlebars>,), Error = Infallible> + Clone {
        warp::any().map(move || Arc::clone(&handlebars))
    }
}

mod handlers {
    use super::templates;

    use std::convert::Infallible;
    use std::path::PathBuf;
    use std::sync::Arc;

    use anyhow::Context;
    use handlebars::Handlebars;
    use serde::Serialize;
    use tokio::fs::File;
    use tokio::io::AsyncReadExt;
    use tracing::error;
    use warp::http::StatusCode;
    use warp::Reply;

    pub async fn get_client_id(
        client_id_path: PathBuf,
        handlebars: Arc<Handlebars<'_>>,
    ) -> Result<impl warp::Reply, Infallible> {
        get_client_id_handler(client_id_path, handlebars)
            .await
            .map(|r| r.into_response())
            .or_else(|e| {
                error!("{:?}", e);
                Ok(StatusCode::INTERNAL_SERVER_ERROR.into_response())
            })
    }

    async fn get_client_id_handler(
        client_id_path: PathBuf,
        hb: Arc<Handlebars<'_>>,
    ) -> anyhow::Result<impl warp::Reply> {
        let content = match read_client_id_from_file(client_id_path).await {
            Ok(client_id) => render(
                hb,
                templates::CLIENT_ID,
                &templates::ClientIdData { client_id },
            ),
            Err(e) => {
                error!("{:?}", e);
                render(hb, templates::MISSING_CLIENT_ID, &())
            }
        }?;

        Ok(warp::reply::html(content).into_response())
    }

    async fn read_client_id_from_file(client_id_path: PathBuf) -> anyhow::Result<String> {
        let mut file = File::open(&client_id_path).await.with_context(|| {
            format!(
                "Failed to open client id file at path {:?}",
                &client_id_path
            )
        })?;

        let mut buffer = String::new();
        file.read_to_string(&mut buffer)
            .await
            .with_context(|| format!("Failed to read client id from path {:?}", &client_id_path))?;

        Ok(buffer)
    }

    fn render<T: Serialize>(
        handlebars: Arc<Handlebars<'_>>,
        template: &'static str,
        data: &T,
    ) -> anyhow::Result<String> {
        handlebars.render(template, &data).context(template)
    }
}
