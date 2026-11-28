//! 推给前端的事件：事件名、负载与把门面事件转成 Tauri 事件的转发器。
//!
//! petalfall-core 与 UI 框架解耦，只认一个 `mpsc` 发送端作为事件出口（[`EventSink`]）。命令建一个
//! [`CoreEventForwarder`]，把它的发送端交给门面方法，转发任务把收到的每条 [`CoreEvent`] 译成
//! [`CoreEventPayload`] 再 `emit` 出去。负载带 `task_id`：前端下载队列据此把进度归到对应任务，
//! 不带 `task_id`（为 `null`）的事件只属于启动会话。
//!
//! 账户变更不属于哪一条命令，后台续期与启动时都会发，另走门面上的广播通道，由 [`forward_account_changes`] 常驻转发。

use petalfall_core::{CoreEvent, DownloadProgress, EventSink};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// 阶段、告警、下载进度、多步任务的步骤与自更新的换源。
pub const CORE_EVENT: &str = "petalfall://core-event";

/// 微软设备码登录拿到设备码时推送 user_code 与验证网址。
pub const DEVICE_CODE_EVENT: &str = "petalfall://device-code";

/// 游戏进程的每一行 stdout / stderr。
pub const GAME_LOG_EVENT: &str = "petalfall://game-log";

/// 后台改写了账户记录（后台提前续期、启动前的验证与续期、启动时顺带同步的外置档案）时推送，负载
/// [`AccountChangedPayload`]。前端重读账户列表，并让这个账户的皮肤查询失效。账户页的刷新、登录、切换与删除由界面
/// 自己发起，做完就重读，不推这个事件。
pub const ACCOUNT_CHANGED_EVENT: &str = "petalfall://account-changed";

/// 推送一个事件；失败（窗口已关、无监听者）只记日志，不影响命令本身的结果。
pub fn emit<S: Serialize + Clone>(app: &AppHandle, event: &str, payload: S) {
    if let Err(err) = app.emit(event, payload) {
        tracing::warn!(event, error = %err, "事件推送失败");
    }
}

/// [`CORE_EVENT`] 的负载：`{ task_id, step?, kind, ... }`。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoreEventPayload {
    /// 所属任务；启动会话与非任务命令的事件为 `null`。
    pub task_id: Option<String>,
    /// 作为组合任务的一项调用时，这是第几项（从 0 计），取自前端调这一项命令时给的 `step`。
    ///
    /// 组合任务逐项调命令、共用一个 `task_id`，每项都从 0 报起；事件与命令的返回走不同的通道，上一项最后几条
    /// 进度可能在下一项开始之后才送到。两项声明大小相同或都未知时，单看快照分不清是谁的，前端按它分。
    /// 单独调用的命令没有这一项，序列化时整个省去，负载与加这个字段之前一模一样。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    #[serde(flatten)]
    pub event: CoreEventKind,
}

/// 按 `kind` 区分的事件主体。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoreEventKind {
    /// 阶段推进（人类可读的一句话）。
    Stage { message: String },
    /// 非阻断性告警。
    Warning { message: String },
    /// 当前这一批下载的进度快照。安装要连跑好几批，每批都从 `finished = 0` 重新计起。
    Download {
        /// 这一批在这次命令里的序号，从 0 计，同一批的快照同一个号（见 [`BatchNumbering`]）。前端按它分批：先后两批
        /// 文件数与声明大小都相同时（整合包文件整批下载失败、换备用地址重下的那一轮），单看快照分不清。
        batch: u32,
        /// 本批文件数。
        total: u64,
        /// 已结束文件数（成功与最终失败都计入）。
        finished: u64,
        /// 有效字节：已就绪的字节，不含重传；声明大小与实际不符时仍可能越过 `total_bytes`，画进度时要封顶。
        bytes: u64,
        /// 本批声明大小之和；有文件大小未知时为 `null`。
        total_bytes: Option<u64>,
        /// 传输字节：网络上实际读到的字节，含重传，跳过的完整文件不算；批内只增不减，本地任务恒为 0。
        transferred: u64,
        /// 瞬时速度，字节/秒。
        speed: u64,
    },
    /// 进入多步任务的某一步：`plan` 是整份步骤表（各步的键），`index` 是这一步的下标。
    Phase {
        plan: Vec<&'static str>,
        index: usize,
    },
    /// 自更新开始从某个下载源取更新包：`index` 是它在本次候选表里的下标（从 0 计），`count` 是候选总数，
    /// `name` 是源名。每个源开始前一条，第一个也有；换源后从头下，之后的下载快照从 0 报起。
    Source {
        index: usize,
        count: usize,
        name: String,
    },
}

impl CoreEventKind {
    /// 译一条门面事件。下载快照的批次号要看同一次命令之前的快照，`batches` 一次命令用一个。
    fn translate(event: CoreEvent, batches: &mut BatchNumbering) -> Self {
        match event {
            CoreEvent::Stage(message) => CoreEventKind::Stage { message },
            CoreEvent::Warning(message) => CoreEventKind::Warning { message },
            CoreEvent::Download(p) => CoreEventKind::Download {
                batch: batches.number(p),
                total: p.total,
                finished: p.finished,
                bytes: p.bytes,
                total_bytes: p.total_bytes,
                transferred: p.transferred,
                speed: p.speed,
            },
            CoreEvent::Phase { plan, index } => CoreEventKind::Phase {
                plan: plan.into_iter().map(|phase| phase.key()).collect(),
                index,
            },
            CoreEvent::Source { index, count, name } => {
                CoreEventKind::Source { index, count, name }
            }
        }
    }
}

/// 记着最近几批，见 [`BatchNumbering`]。前后两个池的快照只会交错一小段，留些余量。
const RECENT_BATCHES: usize = 4;

/// 给一次命令的下载快照编批次号。
///
/// 下载池不给批次编号，这里按池的约定认：同一批的文件数与声明大小不变，已结束文件数与传输字节只增不减。接不上最近
/// 几批里任何一批的快照就是新的一批。记最近几批而不只是上一批：导入整合包装游戏与下整合包文件各用各的池，前一个池
/// 最后一条快照可能夹在后一个池的快照中间才到，只记上一批的话，它与它后面那条都会被当成新的一批。
///
/// 认不出的只有一种：新的一批与之前某一批文件数、声明大小都相同，节流又正好挤掉了那一批的末尾与这一批的开头，
/// 这一批送到的第一条快照里已结束文件数与传输字节都不比那一批最后送到的少。这时这一批已经追到那一批停下的地方，
/// 当成同一批只进不退，界面最多停到它追上为止。
#[derive(Debug, Default)]
struct BatchNumbering {
    /// 最近几批的批次号与它们最后一条快照，新开的在后。
    recent: std::collections::VecDeque<(u32, DownloadProgress)>,
    next: u32,
}

impl BatchNumbering {
    fn number(&mut self, progress: DownloadProgress) -> u32 {
        if let Some((batch, last)) = self
            .recent
            .iter_mut()
            .rev()
            .find(|(_, last)| continues_batch(last, &progress))
        {
            *last = progress;
            return *batch;
        }
        let batch = self.next;
        self.next += 1;
        if self.recent.len() == RECENT_BATCHES {
            self.recent.pop_front();
        }
        self.recent.push_back((batch, progress));
        batch
    }
}

/// `next` 能不能是 `last` 之后同一批的快照。有效字节不算：失败的文件退回时它会变少，那还是同一批。
fn continues_batch(last: &DownloadProgress, next: &DownloadProgress) -> bool {
    next.total == last.total
        && next.total_bytes == last.total_bytes
        && next.finished >= last.finished
        && next.transferred >= last.transferred
}

/// [`ACCOUNT_CHANGED_EVENT`] 的负载：改了的账户。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountChangedPayload {
    pub uuid: String,
}

/// 把门面的账户变更通知（[`petalfall_core::Petalfall::subscribe_account_changes`]）逐条交给 `deliver`，直到门面全部
/// 放下、通道关闭。积压挤掉了几条时记 warn 接着转：丢的那几个账户要等下一次重读才换上新名字与皮肤。
pub fn forward_account_changes(
    mut changes: tokio::sync::broadcast::Receiver<String>,
    deliver: impl Fn(AccountChangedPayload) + Send + 'static,
) -> tauri::async_runtime::JoinHandle<()> {
    use tokio::sync::broadcast::error::RecvError;
    tauri::async_runtime::spawn(async move {
        loop {
            match changes.recv().await {
                Ok(uuid) => deliver(AccountChangedPayload { uuid }),
                Err(RecvError::Lagged(missed)) => tracing::warn!(
                    missed,
                    "账户变更通知积压，挤掉了几条：那几个账户的界面要等下一次重读才更新"
                ),
                Err(RecvError::Closed) => break,
            }
        }
    })
}

/// 转发到前端的 [`ACCOUNT_CHANGED_EVENT`]。订阅要在门面开始后台续期之前拿好，见 [`forward_account_changes`]。
pub fn forward_account_changes_to_app(
    app: &AppHandle,
    changes: tokio::sync::broadcast::Receiver<String>,
) {
    let app = app.clone();
    forward_account_changes(changes, move |payload| {
        emit(&app, ACCOUNT_CHANGED_EVENT, payload)
    });
}

/// 一次命令的门面事件转发：持有交给门面的发送端与后台转发任务。
pub struct CoreEventForwarder {
    sink: EventSink,
    task: tauri::async_runtime::JoinHandle<()>,
}

impl CoreEventForwarder {
    /// 用任意投递函数起转发任务；每条事件都标上 `task_id`。
    pub fn spawn(
        task_id: Option<String>,
        deliver: impl Fn(CoreEventPayload) + Send + 'static,
    ) -> Self {
        Self::spawn_step(task_id, None, deliver)
    }

    /// 同 [`spawn`](Self::spawn)，每条事件另标上组合任务的项序号 `step`（见 [`CoreEventPayload::step`]）。
    pub fn spawn_step(
        task_id: Option<String>,
        step: Option<u32>,
        deliver: impl Fn(CoreEventPayload) + Send + 'static,
    ) -> Self {
        let (sink, mut rx) = tokio::sync::mpsc::unbounded_channel::<CoreEvent>();
        let task = tauri::async_runtime::spawn(async move {
            let mut batches = BatchNumbering::default();
            // 发送端全部释放后 recv 返回 None，循环自然结束。
            while let Some(event) = rx.recv().await {
                deliver(CoreEventPayload {
                    task_id: task_id.clone(),
                    step,
                    event: CoreEventKind::translate(event, &mut batches),
                });
            }
        });
        Self { sink, task }
    }

    /// 转发到前端的 [`CORE_EVENT`]。
    pub fn to_app(app: &AppHandle, task_id: Option<String>) -> Self {
        let app = app.clone();
        Self::spawn(task_id, move |payload| emit(&app, CORE_EVENT, payload))
    }

    /// 同 [`to_app`](Self::to_app)，每条事件另标上组合任务的项序号 `step`。
    pub fn to_app_step(app: &AppHandle, task_id: Option<String>, step: Option<u32>) -> Self {
        let app = app.clone();
        Self::spawn_step(task_id, step, move |payload| {
            emit(&app, CORE_EVENT, payload)
        })
    }

    /// 交给门面方法的事件发送端。
    pub fn sink(&self) -> &EventSink {
        &self.sink
    }

    /// 放掉自己的发送端并等转发任务排空剩余事件。
    ///
    /// 门面内部的进度转发也持有发送端，会在下载池释放后发完最后一份快照才放手，这里会等到它。
    pub async fn finish(self) {
        drop(self.sink);
        if let Err(err) = self.task.await {
            tracing::error!(error = %err, "事件转发任务异常结束");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petalfall_core::DownloadProgress;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[test]
    fn payload_serializes_flat_with_task_id_and_kind() {
        let payload = CoreEventPayload {
            task_id: Some("task-7".into()),
            step: None,
            event: CoreEventKind::Download {
                batch: 2,
                total: 3,
                finished: 1,
                bytes: 2048,
                total_bytes: Some(4096),
                transferred: 1024,
                speed: 512,
            },
        };
        assert_eq!(
            serde_json::to_value(&payload).unwrap(),
            json!({
                "task_id": "task-7",
                "kind": "download",
                "batch": 2,
                "total": 3,
                "finished": 1,
                "bytes": 2048,
                "total_bytes": 4096,
                "transferred": 1024,
                "speed": 512
            })
        );

        let session = CoreEventPayload {
            task_id: None,
            step: None,
            event: CoreEventKind::Stage {
                message: "开始安装原版 1.21".into(),
            },
        };
        assert_eq!(
            serde_json::to_value(&session).unwrap(),
            json!({ "task_id": null, "kind": "stage", "message": "开始安装原版 1.21" })
        );
    }

    /// 步骤事件带整份步骤表的键与下标，前端按键取文案、按下标写「第几步，共几步」。
    #[test]
    fn phase_payload_carries_the_plan_keys_and_index() {
        use petalfall_core::InstallPhase;
        let payload = CoreEventPayload {
            task_id: Some("t#1".into()),
            step: None,
            event: CoreEventKind::translate(
                CoreEvent::Phase {
                    plan: vec![
                        InstallPhase::Metadata,
                        InstallPhase::Libraries,
                        InstallPhase::Assets,
                        InstallPhase::Natives,
                    ],
                    index: 2,
                },
                &mut BatchNumbering::default(),
            ),
        };
        assert_eq!(
            serde_json::to_value(&payload).unwrap(),
            json!({
                "task_id": "t#1",
                "kind": "phase",
                "plan": ["metadata", "libraries", "assets", "natives"],
                "index": 2
            })
        );
    }

    /// 换源事件与其他事件一样平铺在负载里：前端按 kind 认出它，知道换了源、之后的下载进度从 0 报起。
    #[test]
    fn source_payload_carries_index_count_and_name() {
        let payload = CoreEventPayload {
            task_id: Some("update#3".into()),
            step: None,
            event: CoreEventKind::translate(
                CoreEvent::Source {
                    index: 1,
                    count: 4,
                    name: "hk".into(),
                },
                &mut BatchNumbering::default(),
            ),
        };
        assert_eq!(
            serde_json::to_value(&payload).unwrap(),
            json!({
                "task_id": "update#3",
                "kind": "source",
                "index": 1,
                "count": 4,
                "name": "hk"
            })
        );
    }

    #[tokio::test]
    async fn forwarder_tags_every_event_with_task_id_in_order() {
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let sink_log = delivered.clone();
        let forwarder = CoreEventForwarder::spawn(Some("install-1".into()), move |p| {
            sink_log.lock().unwrap().push(p)
        });
        forwarder.sink().send(CoreEvent::stage("开始")).unwrap();
        forwarder
            .sink()
            .send(CoreEvent::Download(DownloadProgress {
                total: 2,
                finished: 2,
                bytes: 10,
                total_bytes: None,
                transferred: 7,
                speed: 0,
            }))
            .unwrap();
        forwarder.sink().send(CoreEvent::warning("慢")).unwrap();
        forwarder.finish().await;

        let delivered = delivered.lock().unwrap();
        assert_eq!(delivered.len(), 3);
        assert!(delivered
            .iter()
            .all(|p| p.task_id.as_deref() == Some("install-1") && p.step.is_none()));
        assert_eq!(
            delivered[1].event,
            CoreEventKind::Download {
                batch: 0,
                total: 2,
                finished: 2,
                bytes: 10,
                total_bytes: None,
                transferred: 7,
                speed: 0,
            }
        );
        assert_eq!(
            delivered[2].event,
            CoreEventKind::Warning {
                message: "慢".into()
            }
        );
    }

    /// finish 要等所有发送端都放手：门面里还持有的克隆发出的最后一条事件不能丢。
    #[tokio::test]
    async fn finish_waits_for_cloned_senders() {
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let sink_log = delivered.clone();
        let forwarder = CoreEventForwarder::spawn(None, move |p| sink_log.lock().unwrap().push(p));
        let late = forwarder.sink().clone();
        let straggler = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            late.send(CoreEvent::stage("最后一条")).unwrap();
        });
        forwarder.finish().await;
        straggler.await.unwrap();

        let delivered = delivered.lock().unwrap();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].task_id, None);
    }

    /// 组合任务的一项：`step` 与 `task_id` 并列，第 0 项也照写；单独调用不带这个键（见上面的形状测试）。
    #[test]
    fn composite_item_payload_carries_step_beside_task_id() {
        let download = CoreEventPayload {
            task_id: Some("all#1".into()),
            step: Some(0),
            event: CoreEventKind::Download {
                batch: 0,
                total: 1,
                finished: 0,
                bytes: 0,
                total_bytes: None,
                transferred: 0,
                speed: 0,
            },
        };
        assert_eq!(
            serde_json::to_value(&download).unwrap(),
            json!({
                "task_id": "all#1",
                "step": 0,
                "kind": "download",
                "batch": 0,
                "total": 1,
                "finished": 0,
                "bytes": 0,
                "total_bytes": null,
                "transferred": 0,
                "speed": 0
            })
        );
        let stage = CoreEventPayload {
            task_id: Some("all#1".into()),
            step: Some(u32::MAX),
            event: CoreEventKind::Stage {
                message: "准备更新".into(),
            },
        };
        assert_eq!(
            serde_json::to_value(&stage).unwrap(),
            json!({ "task_id": "all#1", "step": u32::MAX, "kind": "stage", "message": "准备更新" })
        );
    }

    fn progress(
        total: u64,
        finished: u64,
        bytes: u64,
        total_bytes: Option<u64>,
        transferred: u64,
    ) -> DownloadProgress {
        DownloadProgress {
            total,
            finished,
            bytes,
            total_bytes,
            transferred,
            speed: 0,
        }
    }

    /// 整合包文件整批下载失败、换备用地址重下：第二轮与第一轮文件数、声明大小都一样，已结束文件数与传输字节从头报起，
    /// 换一个批次号；同一轮里失败的文件退回、有效字节变少，还是同一批。
    #[test]
    fn a_retry_round_of_the_same_shape_gets_a_new_batch() {
        let mut batches = BatchNumbering::default();
        let rounds = [
            progress(3, 0, 0, Some(3000), 0),
            progress(3, 0, 1800, Some(3000), 1800),
            // 主地址断流，下到一半的文件退回
            progress(3, 2, 600, Some(3000), 2100),
            progress(3, 3, 0, Some(3000), 2100),
            // 换备用地址重下的那一轮
            progress(3, 0, 0, Some(3000), 0),
            progress(3, 1, 1000, Some(3000), 1000),
            progress(3, 3, 3000, Some(3000), 3000),
        ];
        let numbered: Vec<u32> = rounds.into_iter().map(|p| batches.number(p)).collect();
        assert_eq!(numbered, [0, 0, 0, 0, 1, 1, 1]);

        // 边界：节流挤掉了第一轮的末尾，第二轮第一条只比第一轮最后送到的那条少了传输字节，照样认得出
        let mut batches = BatchNumbering::default();
        assert_eq!(batches.number(progress(1, 0, 900, Some(1000), 900)), 0);
        assert_eq!(batches.number(progress(1, 0, 300, Some(1000), 300)), 1);
    }

    /// 前一个池的最后一条快照夹在后一个池的快照中间才到：它认回自己那一批，后一个池接着用原来的号，不当成新的一批。
    /// 记不住的旧批次（超出 RECENT_BATCHES）再来快照时给新号。
    #[test]
    fn a_straggler_from_an_earlier_pool_keeps_its_own_batch() {
        let mut batches = BatchNumbering::default();
        assert_eq!(batches.number(progress(40, 30, 40, Some(50), 40)), 0);
        assert_eq!(batches.number(progress(7, 2, 10, None, 10)), 1);
        assert_eq!(batches.number(progress(40, 40, 50, Some(50), 50)), 0);
        assert_eq!(batches.number(progress(7, 3, 15, None, 15)), 1);

        let first = progress(1, 1, 1, Some(1), 1);
        let mut batches = BatchNumbering::default();
        assert_eq!(batches.number(first), 0);
        for n in 1..=RECENT_BATCHES as u64 {
            batches.number(progress(n + 1, 0, 0, None, 0));
        }
        assert_eq!(batches.number(first), RECENT_BATCHES as u32 + 1);
    }

    /// 转发器给每条下载快照标上批次号，一次命令一套号；组合任务每一项各有自己的转发器，各自从 0 编起。
    #[tokio::test]
    async fn forwarder_numbers_download_batches_per_command() {
        let log = Log::default();
        for step in 0..2 {
            let forwarder = item_forwarder(&log, step);
            for p in [
                progress(2, 2, 10, Some(10), 10),
                progress(2, 0, 0, Some(10), 0),
                progress(2, 2, 10, Some(10), 10),
            ] {
                forwarder.sink().send(CoreEvent::Download(p)).unwrap();
            }
            forwarder.finish().await;
        }
        let log = log.lock().unwrap();
        let numbered: Vec<(Option<u32>, u32)> = log
            .iter()
            .filter_map(|p| match p.event {
                CoreEventKind::Download { batch, .. } => Some((p.step, batch)),
                _ => None,
            })
            .collect();
        assert_eq!(
            numbered,
            [
                (Some(0), 0),
                (Some(0), 1),
                (Some(0), 1),
                (Some(1), 0),
                (Some(1), 1),
                (Some(1), 1)
            ]
        );
    }

    fn account_change_log() -> (
        Arc<Mutex<Vec<AccountChangedPayload>>>,
        impl Fn(AccountChangedPayload) + Send + 'static,
    ) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = log.clone();
        (log, move |payload| sink.lock().unwrap().push(payload))
    }

    /// 门面在后台改了哪个账户，转发出去的负载就是 `{ uuid }`，按发生的先后；门面全部放下、通道关闭后转发任务自己结束。
    #[tokio::test]
    async fn account_changes_are_forwarded_in_order_until_the_channel_closes() {
        let (changes, receiver) = tokio::sync::broadcast::channel(8);
        let (log, deliver) = account_change_log();
        let forwarder = forward_account_changes(receiver, deliver);
        for uuid in ["1111aaaa", "2222bbbb", "1111aaaa"] {
            changes.send(uuid.to_owned()).unwrap();
        }
        drop(changes);
        tokio::time::timeout(std::time::Duration::from_secs(5), forwarder)
            .await
            .expect("通道关闭后转发任务应结束")
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(
            log.iter().map(|p| p.uuid.as_str()).collect::<Vec<_>>(),
            ["1111aaaa", "2222bbbb", "1111aaaa"]
        );
        assert_eq!(
            serde_json::to_value(&log[1]).unwrap(),
            json!({ "uuid": "2222bbbb" })
        );
    }

    /// 积压挤掉了几条：记下之后接着转后面的，不停下。改成遇到任何接收错误就退出，后面两条就转不出去。
    #[tokio::test]
    async fn lagged_account_changes_keep_forwarding_the_rest() {
        let (changes, receiver) = tokio::sync::broadcast::channel(2);
        for uuid in ["a1", "b2", "c3", "d4"] {
            changes.send(uuid.to_owned()).unwrap();
        }
        drop(changes);
        let (log, deliver) = account_change_log();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            forward_account_changes(receiver, deliver),
        )
        .await
        .expect("通道关闭后转发任务应结束")
        .unwrap();

        assert_eq!(
            *log.lock().unwrap(),
            vec![
                AccountChangedPayload { uuid: "c3".into() },
                AccountChangedPayload { uuid: "d4".into() }
            ]
        );
    }

    /// 前端按同一个事件名订阅；两边各写一份，改了一边这里就失败。
    #[test]
    fn front_end_subscribes_to_the_same_account_changed_event() {
        let ts = include_str!("../../src/ipc/accounts.ts");
        assert!(
            ts.contains(&format!(
                "export const ACCOUNT_CHANGED_EVENT = \"{ACCOUNT_CHANGED_EVENT}\";"
            )),
            "app/src/ipc/accounts.ts 里的事件名与后端不一致"
        );
    }

    type Log = Arc<Mutex<Vec<CoreEventPayload>>>;

    /// 组合任务「all#1」第 `step` 项的转发：几项共用同一个 task_id，各用各的转发器，全都投进同一份记录，
    /// 与前端从同一个事件名上收到的一样。
    fn item_forwarder(log: &Log, step: u32) -> CoreEventForwarder {
        let log = log.clone();
        CoreEventForwarder::spawn_step(Some("all#1".into()), Some(step), move |p| {
            log.lock().unwrap().push(p)
        })
    }

    fn snapshot(finished: u64, bytes: u64, total_bytes: Option<u64>) -> CoreEvent {
        CoreEvent::Download(DownloadProgress {
            total: 1,
            finished,
            bytes,
            total_bytes,
            transferred: bytes,
            speed: 0,
        })
    }

    /// 记录里的下载快照：(项序号, 已下字节)。
    fn downloads(log: &Log) -> Vec<(Option<u32>, u64)> {
        let log = log.lock().unwrap();
        assert!(log.iter().all(|p| p.task_id.as_deref() == Some("all#1")));
        log.iter()
            .filter_map(|p| match p.event {
                CoreEventKind::Download { bytes, .. } => Some((p.step, bytes)),
                _ => None,
            })
            .collect()
    }

    /// 两项按顺序各跑一遍同一串快照，返回整份记录。
    async fn run_two_items(snapshots: &[(u64, u64, Option<u64>)]) -> Log {
        let log = Log::default();
        for step in 0..2 {
            let forwarder = item_forwarder(&log, step);
            for &(finished, bytes, total_bytes) in snapshots {
                forwarder
                    .sink()
                    .send(snapshot(finished, bytes, total_bytes))
                    .unwrap();
            }
            forwarder.finish().await;
        }
        log
    }

    /// 两项声明大小相同：两项的快照逐字段相同，只有 step 分得开。
    #[tokio::test]
    async fn items_of_equal_size_differ_only_in_step() {
        let log =
            run_two_items(&[(0, 0, Some(100)), (0, 50, Some(100)), (1, 100, Some(100))]).await;
        assert_eq!(
            downloads(&log),
            vec![
                (Some(0), 0),
                (Some(0), 50),
                (Some(0), 100),
                (Some(1), 0),
                (Some(1), 50),
                (Some(1), 100)
            ]
        );
        let log = log.lock().unwrap();
        for i in 0..3 {
            assert_eq!(
                log[i].event,
                log[i + 3].event,
                "第 {i} 条快照两项应完全相同"
            );
        }
    }

    /// 两项大小都未知：总量为 null、已下字节各自从 0 涨起，同样只有 step 分得开。
    #[tokio::test]
    async fn items_of_unknown_size_differ_only_in_step() {
        let log = run_two_items(&[(0, 0, None), (0, 700, None), (1, 1500, None)]).await;
        assert_eq!(
            downloads(&log),
            vec![
                (Some(0), 0),
                (Some(0), 700),
                (Some(0), 1500),
                (Some(1), 0),
                (Some(1), 700),
                (Some(1), 1500)
            ]
        );
    }

    async fn wait_delivered(log: &Log, count: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while log.lock().unwrap().len() < count {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("转发任务没有在 5 秒内送达");
    }

    /// 上一项的最后一条快照在下一项已经报过进度之后才送出：照旧标着上一项的序号，不会被下一项认领。
    #[tokio::test]
    async fn late_snapshot_of_previous_item_keeps_its_own_step() {
        let log = Log::default();
        let first = item_forwarder(&log, 0);
        let straggler = first.sink().clone();
        first.sink().send(snapshot(0, 60, Some(100))).unwrap();
        wait_delivered(&log, 1).await;

        let second = item_forwarder(&log, 1);
        second.sink().send(snapshot(0, 30, Some(300))).unwrap();
        wait_delivered(&log, 2).await;
        straggler.send(snapshot(1, 100, Some(100))).unwrap();
        drop(straggler);
        first.finish().await;
        second.sink().send(snapshot(1, 300, Some(300))).unwrap();
        second.finish().await;

        assert_eq!(
            downloads(&log),
            vec![(Some(0), 60), (Some(1), 30), (Some(0), 100), (Some(1), 300)]
        );
    }
}
