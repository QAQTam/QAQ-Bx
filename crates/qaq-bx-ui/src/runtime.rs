//! 进程级异步运行时与并发限流。
//!
//! 取代原先各处每次 `spawn_background` 自建一套 tokio runtime 的做法：
//! - 全进程共享单个轻量 tokio multi_thread runtime（2 worker，贴合 4 核 E3-1230 V2 / 8GB 约束）；
//! - 封面异步下载经 `Semaphore` 限流（最多 4 路并发），避免翻页瞬时拉满连接与位图内存。

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::Runtime;
use tokio::sync::Semaphore;

const ASYNC_WORKERS: usize = 2;
const COVER_CONCURRENCY: usize = 4;

static TOKIO_RT: OnceLock<Runtime> = OnceLock::new();
static COVER_SEM: OnceLock<Semaphore> = OnceLock::new();

/// 获取进程唯一共享的 tokio runtime。
pub fn shared_runtime() -> &'static Runtime {
    TOKIO_RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(ASYNC_WORKERS)
            .thread_name("qaq-bx-async")
            .enable_all()
            .build()
            .expect("构建共享 tokio runtime 失败")
    })
}

/// 在后台线程池阻塞驱动异步任务（共享单一 runtime）。
pub fn block_on<F: Future>(future: F) -> F::Output {
    shared_runtime().block_on(future)
}

/// 封面下载并发信号量（最多 4 路同时下载）。
pub fn cover_semaphore() -> &'static Semaphore {
    COVER_SEM.get_or_init(|| Semaphore::new(COVER_CONCURRENCY))
}
