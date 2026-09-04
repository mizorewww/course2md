import { useCallback, useEffect, useRef, useState } from "react";
import type { DoneStats, EnvInfo, JobEvent, JobState, ResultData } from "./types";
import { detectEnvironment, onJobEvent, onJobExit, readResult } from "./lib/backend";
import { errText, nowTime } from "./lib/utils";
import { ToastProvider } from "./components/Toast";
import { Sidebar, type NavId } from "./components/Sidebar";
import { ResultView } from "./components/ResultView";
import { NewJobView } from "./views/NewJobView";
import { RunningView } from "./views/RunningView";
import { HistoryView } from "./views/HistoryView";
import { SettingsView } from "./views/SettingsView";

type View =
  | { kind: "new-job" }
  | { kind: "running" }
  | { kind: "result" }
  | { kind: "history" }
  | { kind: "settings" };

interface ResultState {
  outDir: string;
  data: ResultData | null;
  stats: DoneStats | null;
}

function newJobState(jobId: string): JobState {
  return { jobId, stages: {}, progress: null, logs: [], error: null, cancelled: false };
}

function App() {
  const [view, setView] = useState<View>({ kind: "new-job" });
  const [env, setEnv] = useState<EnvInfo | null>(null);
  const [envError, setEnvError] = useState<string | null>(null);
  const [job, setJob] = useState<JobState | null>(null);
  const [result, setResult] = useState<ResultState | null>(null);
  const logSeq = useRef(0);
  /** 当前活动 jobId 的 ref：事件回调里用它过滤，避免依赖 state 闭包 */
  const jobIdRef = useRef<string | null>(null);

  // M3：环境检测三态——加载中（env=null, envError=null）/ 失败（envError）/ 成功（env）
  const refreshEnv = useCallback(() => {
    detectEnvironment()
      .then((e) => {
        setEnv(e);
        setEnvError(null);
      })
      .catch((e) => {
        setEnv(null);
        setEnvError(errText(e));
      });
  }, []);

  useEffect(() => {
    refreshEnv();
  }, [refreshEnv]);

  const loadResult = useCallback((outDir: string, stats: DoneStats | null) => {
    setResult({ outDir, data: null, stats });
    setView({ kind: "result" });
    readResult(outDir)
      .then((data) => setResult((r) => (r && r.outDir === outDir ? { ...r, data } : r)))
      .catch(() => undefined);
  }, []);

  // 全局 job 事件订阅（App 生命周期内只挂一次），按 job_id 过滤
  useEffect(() => {
    let unEvent: (() => void) | undefined;
    let unExit: (() => void) | undefined;
    let disposed = false;

    const handleEvent = (jobId: string, event: JobEvent) => {
      if (jobIdRef.current !== jobId) return;
      // M9：兼容后端的批量形态（{type:"logs", logs:[...]}）与单条形态
      const events: JobEvent[] = event.type === "logs" ? event.logs : [event];
      for (const ev of events) {
        if (ev.type === "done") {
          // 任务完结：清掉 job（「进行中」导航项随之消失），再展示结果
          jobIdRef.current = null;
          setJob(null);
          loadResult(ev.out_dir, {
            outDir: ev.out_dir,
            title: ev.title,
            slides: ev.slides,
            segments: ev.segments,
            chars: ev.chars,
            elapsedSecs: ev.elapsed_secs,
            outputs: ev.outputs,
          });
          return;
        }
      }
      setJob((prev) => {
        if (!prev || prev.jobId !== jobId) return prev;
        let next = prev;
        for (const ev of events) {
          switch (ev.type) {
            case "log":
              next = {
                ...next,
                logs: [
                  ...next.logs.slice(-499),
                  { id: ++logSeq.current, level: ev.level, message: ev.message, time: nowTime() },
                ],
              };
              break;
            case "stage":
              // 新阶段开始时清掉上一阶段残留的进度条
              next = {
                ...next,
                stages: { ...next.stages, [ev.stage]: ev.status },
                progress: ev.status === "start" ? null : next.progress,
              };
              break;
            case "progress":
              next = {
                ...next,
                progress: {
                  stage: ev.stage,
                  current: ev.current,
                  total: ev.total,
                  message: ev.message,
                },
              };
              break;
            case "error":
              next = { ...next, error: ev.message };
              break;
            default:
              break;
          }
        }
        return next;
      });
    };

    const handleExit = (jobId: string, code: number | null) => {
      if (jobIdRef.current !== jobId) return;
      setJob((prev) => {
        if (!prev || prev.jobId !== jobId) return prev;
        if (prev.error || prev.cancelled) return prev;
        // S5：错误横幅优先展示最近的 error 级日志内容（启动前失败等场景只有 stderr）
        const lastErrLog = [...prev.logs].reverse().find((l) => l.level === "error")?.message;
        // S2：退出码 None = 被信号杀死（取消时已先置 cancelled，走不到这里）
        if (code === null) {
          return { ...prev, error: lastErrLog ?? "进程被信号终止（可能 OOM 或崩溃）" };
        }
        if (code !== 0) {
          return { ...prev, error: lastErrLog ?? `进程异常退出（退出码 ${code}）` };
        }
        return prev;
      });
    };

    onJobEvent((id, e) => {
      if (!disposed) handleEvent(id, e);
    }).then((u) => {
      if (disposed) u();
      else unEvent = u;
    });
    onJobExit((id, code) => {
      if (!disposed) handleExit(id, code);
    }).then((u) => {
      if (disposed) u();
      else unExit = u;
    });

    return () => {
      disposed = true;
      unEvent?.();
      unExit?.();
    };
  }, [loadResult]);

  const startJobView = useCallback((jobId: string) => {
    jobIdRef.current = jobId;
    setJob(newJobState(jobId));
    setView({ kind: "running" });
  }, []);

  // S1：任务进行中（未失败/未取消/未完结）——侧栏显示「进行中」导航项
  const jobActive = job !== null && !job.error && !job.cancelled;

  const navigate = (id: NavId) => {
    if (id === "running" && !job) return;
    setView({ kind: id });
  };

  // 结果页高亮哪个导航：从历史进来的（无 stats）高亮「历史」，否则高亮「新建任务」
  const activeNav: NavId =
    view.kind === "running"
      ? "running"
      : view.kind === "history" || view.kind === "settings" || view.kind === "new-job"
        ? view.kind
        : view.kind === "result" && result?.stats == null
          ? "history"
          : "new-job";

  return (
    <ToastProvider>
      <div className="flex h-full bg-zinc-950 text-zinc-200">
        <Sidebar
          active={activeNav}
          onNavigate={navigate}
          env={env}
          envError={envError}
          onRefreshEnv={refreshEnv}
          running={jobActive}
        />
        <main className="min-w-0 flex-1">
          {view.kind === "new-job" && (
            <NewJobView env={env} jobRunning={jobActive} onStarted={startJobView} />
          )}
          {view.kind === "running" && job && (
            <RunningView
              job={job}
              onCancel={() => setJob((j) => (j ? { ...j, cancelled: true } : j))}
              onBack={() => {
                jobIdRef.current = null;
                setJob(null);
                setView({ kind: "new-job" });
              }}
            />
          )}
          {view.kind === "result" && result && (
            <ResultView
              outDir={result.outDir}
              data={result.data}
              stats={result.stats}
              onBack={() => setView(result.stats ? { kind: "new-job" } : { kind: "history" })}
            />
          )}
          {view.kind === "history" && <HistoryView onOpen={(dir) => loadResult(dir, null)} />}
          {view.kind === "settings" && <SettingsView />}
        </main>
      </div>
    </ToastProvider>
  );
}

export default App;
