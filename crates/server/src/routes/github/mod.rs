mod issues;

use axum::Router;

use crate::DeploymentImpl;

pub fn router() -> Router<DeploymentImpl> {
    Router::new().merge(issues::router())
}
