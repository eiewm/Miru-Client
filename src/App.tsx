import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { motion } from 'framer-motion'
import {
  Activity,
  ChevronDown,
  ChevronRight,
  Cpu,
  Download,
  FileClock,
  Gauge,
  History,
  ListFilter,
  LoaderCircle,
  LogIn,
  LogOut,
  MonitorPlay,
  Play,
  RadioTower,
  Settings,
  Square,
  Terminal,
  UserRound,
} from 'lucide-react'
import { useCallback, useEffect, useRef, useState, startTransition } from 'react'
import type { ReactNode } from 'react'
import './index.css'

type Resolution = 'p720' | 'p1080'
type WatcherStatus = 'stopped' | 'starting' | 'running' | 'error'
type WorkerStatus = 'disconnected' | 'connecting' | 'connected' | 'error'
type TabKey = 'dashboard' | 'auto' | 'server' | 'history' | 'settings' | 'logs'

type NumericRuleOp = 'eq' | 'gte' | 'lte' | 'between'

interface NumericRule {
  enabled: boolean
  op: NumericRuleOp
  value: number
  maxValue?: number | null
}

interface JudgmentRules {
  max: NumericRule
  n300: NumericRule
  n200: NumericRule
  n100: NumericRule
  n50: NumericRule
  miss: NumericRule
}

interface AutoRendererConfig {
  source: 'osuStable'
  osuStablePathOverride: string
  selectedPresetId?: string | null
  selectedSkinId: string
  keyCounts: number[]
  longNoteRule: NumericRule
  normalNoteRule: NumericRule
  totalNoteRule: NumericRule
  maxComboRule: NumericRule
  accuracyRule: NumericRule
  ppRule: NumericRule
  bpmRule: NumericRule
  hpRule: NumericRule
  csRule: NumericRule
  odRule: NumericRule
  durationRule: NumericRule
  judgmentRules: JudgmentRules
}

interface AutoRendererEvent {
  replayName: string
  title: string
  detail: string
  status: string
}

interface AppConfig {
  userId: string
  username: string
  userAvatarUrl: string
  userRole: string
  userPlan: string
  discordLinked: boolean
  apiUrl: string
  frontendUrl: string
  resolution: Resolution
  autoRenderer: AutoRendererConfig
  discord: {
    enabled: boolean
    webhookSet: boolean
  }
  isServer: boolean
  registeredUserId: string
  serverClientId: string
  serverStatus: string
  serverName: string
  serverGpu: string
  serverAutoReconnect: boolean
  showDiscordRendererRole: boolean
  showGpuInStatusImage: boolean
  connectWorkerOnLaunch: boolean
  rendererOverridePath: string
  autostart: boolean
  startMinimizedToTray: boolean
  closeToTrayOnExit: boolean
  importedLegacyConfig: boolean
}

interface BenchmarkProgress {
  phase: string
  percent: number
  message: string
}

interface ManagedToolSnapshot {
  path: string
  directory: string
  exists: boolean
  source: string
}

interface FfmpegToolsSnapshot {
  directory: string
  ffmpeg: ManagedToolSnapshot
  ffprobe: ManagedToolSnapshot
}

interface RuntimeSnapshot {
  isAuthenticated: boolean
  watcherStatus: WatcherStatus
  osuStableDetected: boolean
  replayDirReady: boolean
  stableReplayDirReady: boolean
  songsDirReady: boolean
  osuStableRoot?: string | null
  replayDir?: string | null
  stableReplayDir?: string | null
  songsDir?: string | null
  lastAutoRendererEvent?: AutoRendererEvent | null
  rendererInstalled: boolean
  workerStatus: WorkerStatus
  activeJobId?: string | null
  benchmark?: BenchmarkProgress | null
  lastBenchmark?: BenchmarkResult | null
  ffmpegTools: FfmpegToolsSnapshot
}

interface HistoryEntry {
  id: string
  timestamp: string
  kind: string
  title: string
  detail: string
  status: string
  url?: string | null
}

interface AppStatePayload {
  config: AppConfig
  runtime: RuntimeSnapshot
  history: HistoryEntry[]
  logs: string[]
}

interface AutoRendererLibraryPreset {
  id: string
  name: string
  config?: unknown
  isDefault?: boolean
  updatedAt?: string | null
}

interface AutoRendererLibrarySkin {
  id: string
  name: string
  isDefault?: boolean
  sizeBytes?: number | null
  createdAt?: string | null
}

interface AutoRendererLibrary {
  presets: AutoRendererLibraryPreset[]
  skins: AutoRendererLibrarySkin[]
}

interface BenchmarkResult {
  renderTimeMs: number
  downloadMbps: number
  uploadMbps: number
  latencyMs: number
  speedTestBytes: number
  benchmarkSource: string
  maxRenderMs: number
  minMbps: number
  minUploadMbps: number
  gpuName: string
}

interface DownloadPlanItem {
  name: string
  detail: string
  sizeBytes?: number | null
  willDownload: boolean
  status?: string | null
}

interface BenchmarkDownloadPlan {
  installPath: string
  releaseUrl?: string | null
  items: DownloadPlanItem[]
  totalDownloadBytes: number
}

interface ClientUpdateStatus {
  currentVersion: string
  latestVersion: string
  updateAvailable: boolean
  releaseUrl: string
  downloadUrl: string
  assetName: string
  sizeBytes: number
  sha256: string
  publishedAt?: string | null
  currentCommit?: string | null
  latestCommit?: string
}

interface ClientUpdateProgress {
  phase: string
  percent: number
  message: string
  downloadedBytes?: number | null
  totalBytes?: number | null
}

interface RendererUpdateStatus {
  currentVersion: string
  latestVersion: string
  updateAvailable: boolean
  installed: boolean
  customOverride: boolean
  releaseUrl: string
  assetName: string
  sizeBytes: number
  installPath: string
}

type PendingUpdateRequest =
  | { kind: 'client'; update: ClientUpdateStatus }
  | { kind: 'renderer'; update: RendererUpdateStatus }

interface WorkerComplianceSummary {
  activeSecondsThisWeek: number
  requiredSecondsPerWeek: number
  status: 'grace' | 'ok' | 'behind' | 'inactive' | string
  graceEndsAt?: string | null
  windowStartedAt?: string | null
}

interface WorkerStatsPayload {
  registered: boolean
  status: string
  isOnline: boolean
  name: string
  clientId?: string | null
  jobsCompleted: number
  jobsFailed: number
  totalRenderTimeSeconds: number
  slotsAvailable: number
  slotsTotal: number
  compliance?: WorkerComplianceSummary | null
}

interface WorkerHistoryEntry {
  id: string
  status: string
  replayName?: string | null
  title: string
  difficulty?: string | null
  outputSizeBytes?: number | null
  queuedAt?: string | null
  startedAt?: string | null
  completedAt?: string | null
  durationMs?: number | null
  errorCode?: string | null
  errorMessage?: string | null
}

interface SettingsDraft {
  apiUrl: string
  frontendUrl: string
  resolution: Resolution
  autoRenderer: AutoRendererConfig
  discordEnabled: boolean
  discordWebhook: string
  serverName: string
  rendererOverridePath: string
  autostart: boolean
  startMinimizedToTray: boolean
  showDiscordRendererRole: boolean
  showGpuInStatusImage: boolean
  connectWorkerOnLaunch: boolean
  closeToTrayOnExit: boolean
}

type NumericRuleKey =
  | 'longNoteRule'
  | 'normalNoteRule'
  | 'totalNoteRule'
  | 'maxComboRule'
  | 'accuracyRule'
  | 'ppRule'
  | 'bpmRule'
  | 'hpRule'
  | 'csRule'
  | 'odRule'
  | 'durationRule'

type JudgmentRuleKey = keyof JudgmentRules

const navItems: { key: TabKey; label: string; icon: typeof Activity }[] = [
  { key: 'dashboard', label: 'Dashboard', icon: Gauge },
  { key: 'auto', label: 'Auto Renderer', icon: MonitorPlay },
  { key: 'server', label: 'Server Worker', icon: Cpu },
  { key: 'history', label: 'History', icon: History },
  { key: 'settings', label: 'Settings', icon: Settings },
  { key: 'logs', label: 'Logs', icon: Terminal },
]

const defaultNumericRule: NumericRule = {
  enabled: false,
  op: 'eq',
  value: 0,
  maxValue: null,
}

const defaultJudgmentRules: JudgmentRules = {
  max: { ...defaultNumericRule },
  n300: { ...defaultNumericRule },
  n200: { ...defaultNumericRule },
  n100: { ...defaultNumericRule },
  n50: { ...defaultNumericRule },
  miss: { ...defaultNumericRule },
}

const defaultAutoRendererConfig: AutoRendererConfig = {
  source: 'osuStable',
  osuStablePathOverride: '',
  selectedPresetId: null,
  selectedSkinId: 'default',
  keyCounts: [],
  longNoteRule: { ...defaultNumericRule },
  normalNoteRule: { ...defaultNumericRule },
  totalNoteRule: { ...defaultNumericRule },
  maxComboRule: { ...defaultNumericRule },
  accuracyRule: { ...defaultNumericRule },
  ppRule: { ...defaultNumericRule },
  bpmRule: { ...defaultNumericRule },
  hpRule: { ...defaultNumericRule },
  csRule: { ...defaultNumericRule },
  odRule: { ...defaultNumericRule },
  durationRule: { ...defaultNumericRule },
  judgmentRules: defaultJudgmentRules,
}

const defaultAutoRendererLibrary: AutoRendererLibrary = {
  presets: [],
  skins: [{ id: 'default', name: 'Default skin', isDefault: true }],
}

const MAX_SERVER_NAME_LENGTH = 18

const defaultDraft: SettingsDraft = {
  apiUrl: 'https://app.miru.uno/api/v1',
  frontendUrl: 'https://app.miru.uno',
  resolution: 'p720',
  autoRenderer: defaultAutoRendererConfig,
  discordEnabled: false,
  discordWebhook: '',
  serverName: '',
  rendererOverridePath: '',
  autostart: false,
  startMinimizedToTray: false,
  showDiscordRendererRole: true,
  showGpuInStatusImage: true,
  connectWorkerOnLaunch: true,
  closeToTrayOnExit: true,
}

function normalizeNumericRule(rule: Partial<NumericRule> | undefined): NumericRule {
  return {
    ...defaultNumericRule,
    ...(rule ?? {}),
    enabled: Boolean(rule?.enabled),
    op: rule?.op === 'gte' || rule?.op === 'lte' || rule?.op === 'between' ? rule.op : 'eq',
    value: Number.isFinite(rule?.value) ? Number(rule?.value) : 0,
    maxValue: Number.isFinite(rule?.maxValue) ? Number(rule?.maxValue) : null,
  }
}

function normalizeJudgmentRules(rules: Partial<JudgmentRules> | undefined): JudgmentRules {
  return {
    max: normalizeNumericRule(rules?.max),
    n300: normalizeNumericRule(rules?.n300),
    n200: normalizeNumericRule(rules?.n200),
    n100: normalizeNumericRule(rules?.n100),
    n50: normalizeNumericRule(rules?.n50),
    miss: normalizeNumericRule(rules?.miss),
  }
}

function normalizeEntitlementLabel(value: string | null | undefined): string {
  return value?.trim().toUpperCase() ?? ''
}

function canUseAutoRenderer(config: AppConfig): boolean {
  const role = normalizeEntitlementLabel(config.userRole)
  const plan = normalizeEntitlementLabel(config.userPlan)
  return role === 'PLUS' || role === 'ADMIN' || plan === 'PLUS'
}

function entitlementSummary(config: AppConfig): string {
  const role = normalizeEntitlementLabel(config.userRole) || 'UNKNOWN'
  const plan = normalizeEntitlementLabel(config.userPlan) || 'UNKNOWN'
  return `Detected account: role ${role}, plan ${plan}.`
}

function normalizeAutoRendererConfig(config: Partial<AutoRendererConfig> | undefined): AutoRendererConfig {
  return {
    ...defaultAutoRendererConfig,
    ...(config ?? {}),
    source: 'osuStable',
    osuStablePathOverride: config?.osuStablePathOverride ?? '',
    selectedPresetId: config?.selectedPresetId ?? null,
    selectedSkinId: config?.selectedSkinId?.trim() || 'default',
    keyCounts: Array.isArray(config?.keyCounts) ? config.keyCounts : [],
    longNoteRule: normalizeNumericRule(config?.longNoteRule),
    normalNoteRule: normalizeNumericRule(config?.normalNoteRule),
    totalNoteRule: normalizeNumericRule(config?.totalNoteRule),
    maxComboRule: normalizeNumericRule(config?.maxComboRule),
    accuracyRule: normalizeNumericRule(config?.accuracyRule),
    ppRule: normalizeNumericRule(config?.ppRule),
    bpmRule: normalizeNumericRule(config?.bpmRule),
    hpRule: normalizeNumericRule(config?.hpRule),
    csRule: normalizeNumericRule(config?.csRule),
    odRule: normalizeNumericRule(config?.odRule),
    durationRule: normalizeNumericRule(config?.durationRule),
    judgmentRules: normalizeJudgmentRules(config?.judgmentRules),
  }
}

function normalizeDisplayUsername(username: string): string {
  const trimmed = username.trim()
  return trimmed.toLowerCase() === 'miru user' ? '' : trimmed
}

function normalizeServerName(value: string): string {
  return value.trim().slice(0, MAX_SERVER_NAME_LENGTH)
}

function clampPercent(value: number): number {
  if (!Number.isFinite(value)) return 0
  return Math.max(0, Math.min(100, Math.round(value)))
}

function settingsInputFromDraft(draft: SettingsDraft) {
  return {
    apiUrl: draft.apiUrl,
    frontendUrl: draft.frontendUrl,
    resolution: draft.resolution,
    autoRenderer: normalizeAutoRendererConfig(draft.autoRenderer),
    discordEnabled: draft.discordEnabled,
    discordWebhook: draft.discordWebhook.trim() ? draft.discordWebhook.trim() : null,
    serverName: normalizeServerName(draft.serverName),
    rendererOverridePath: draft.rendererOverridePath,
    autostart: draft.autostart,
    startMinimizedToTray: draft.startMinimizedToTray,
    showDiscordRendererRole: draft.showDiscordRendererRole,
    showGpuInStatusImage: draft.showGpuInStatusImage,
    connectWorkerOnLaunch: draft.connectWorkerOnLaunch,
    closeToTrayOnExit: draft.closeToTrayOnExit,
  }
}

function App() {
  const [appState, setAppState] = useState<AppStatePayload | null>(null)
  const [activeTab, setActiveTab] = useState<TabKey>('dashboard')
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [settingsDraft, setSettingsDraft] = useState<SettingsDraft>(defaultDraft)
  const [serverName, setServerName] = useState('')
  const [lastBenchmark, setLastBenchmark] = useState<BenchmarkResult | null>(null)
  const [slots, setSlots] = useState<number | null>(null)
  const [showRegisterPrompt, setShowRegisterPrompt] = useState(false)
  const [benchmarkPlan, setBenchmarkPlan] = useState<BenchmarkDownloadPlan | null>(null)
  const [workerStats, setWorkerStats] = useState<WorkerStatsPayload | null>(null)
  const [workerHistory, setWorkerHistory] = useState<WorkerHistoryEntry[]>([])
  const [autoRendererLibrary, setAutoRendererLibrary] = useState<AutoRendererLibrary>(defaultAutoRendererLibrary)
  const [autoRendererLibraryError, setAutoRendererLibraryError] = useState<string | null>(null)
  const [clientUpdate, setClientUpdate] = useState<ClientUpdateStatus | null>(null)
  const [clientUpdateError, setClientUpdateError] = useState<string | null>(null)
  const [clientUpdateProgress, setClientUpdateProgress] = useState<ClientUpdateProgress | null>(null)
  const [checkingClientUpdate, setCheckingClientUpdate] = useState(false)
  const [rendererUpdate, setRendererUpdate] = useState<RendererUpdateStatus | null>(null)
  const [rendererUpdateError, setRendererUpdateError] = useState<string | null>(null)
  const [checkingRendererUpdate, setCheckingRendererUpdate] = useState(false)
  const [pendingUpdate, setPendingUpdate] = useState<PendingUpdateRequest | null>(null)
  const lastSavedSettingsPayload = useRef('')
  const settingsSaveRequestId = useRef(0)

  const refreshState = useCallback(async (): Promise<AppStatePayload> => {
    const next = await invoke<AppStatePayload>('get_app_state')
    const nextDraft: SettingsDraft = {
      apiUrl: next.config.apiUrl,
      frontendUrl: next.config.frontendUrl,
      resolution: next.config.resolution,
      autoRenderer: normalizeAutoRendererConfig(next.config.autoRenderer),
      discordEnabled: next.config.discord.enabled,
      discordWebhook: '',
      serverName: normalizeServerName(next.config.serverName),
      rendererOverridePath: next.config.rendererOverridePath,
      autostart: next.config.autostart,
      startMinimizedToTray: next.config.startMinimizedToTray,
      showDiscordRendererRole: next.config.showDiscordRendererRole,
      showGpuInStatusImage: next.config.showGpuInStatusImage,
      connectWorkerOnLaunch: next.config.connectWorkerOnLaunch,
      closeToTrayOnExit: next.config.closeToTrayOnExit,
    }
    lastSavedSettingsPayload.current = JSON.stringify(settingsInputFromDraft(nextDraft))
    startTransition(() => {
      setAppState(next)
      setSettingsDraft(nextDraft)
      setServerName(
        normalizeServerName(next.config.serverName || normalizeDisplayUsername(next.config.username) || 'Miru PC')
      )
      setLastBenchmark(next.runtime.lastBenchmark ?? null)
    })
    if (next.runtime.isAuthenticated) {
      const [statsResult, historyResult, libraryResult] = await Promise.allSettled([
        invoke<WorkerStatsPayload>('get_worker_stats'),
        invoke<WorkerHistoryEntry[]>('get_worker_history'),
        invoke<AutoRendererLibrary>('get_auto_renderer_library'),
      ])
      startTransition(() => {
        if (statsResult.status === 'fulfilled') {
          setWorkerStats(statsResult.value)
          setSlots(statsResult.value.slotsAvailable)
        } else {
          setWorkerStats(null)
        }
        if (historyResult.status === 'fulfilled') {
          setWorkerHistory(historyResult.value)
        } else {
          setWorkerHistory([])
        }
        if (libraryResult.status === 'fulfilled') {
          setAutoRendererLibrary({
            presets: libraryResult.value.presets ?? [],
            skins: libraryResult.value.skins?.length ? libraryResult.value.skins : defaultAutoRendererLibrary.skins,
          })
          setAutoRendererLibraryError(null)
        } else {
          setAutoRendererLibrary(defaultAutoRendererLibrary)
          setAutoRendererLibraryError(libraryResult.reason instanceof Error ? libraryResult.reason.message : String(libraryResult.reason))
        }
      })
      if (statsResult.status === 'rejected' && historyResult.status === 'rejected') {
        startTransition(() => {
          setShowRegisterPrompt(false)
        })
      }
    } else {
      startTransition(() => {
        setWorkerStats(null)
        setWorkerHistory([])
        setAutoRendererLibrary(defaultAutoRendererLibrary)
        setAutoRendererLibraryError(null)
        setShowRegisterPrompt(false)
      })
    }
    return next
  }, [])

  const checkForClientUpdate = useCallback(async (options?: { silent?: boolean }) => {
    setCheckingClientUpdate(true)
    try {
      const next = await invoke<ClientUpdateStatus>('check_client_update')
      startTransition(() => {
        setClientUpdate(next)
        setClientUpdateError(null)
      })
    } catch (err) {
      startTransition(() => {
        setClientUpdate(null)
        setClientUpdateError(options?.silent ? null : err instanceof Error ? err.message : String(err))
      })
    } finally {
      setCheckingClientUpdate(false)
    }
  }, [])

  const checkForRendererUpdate = useCallback(async (options?: { silent?: boolean }) => {
    setCheckingRendererUpdate(true)
    try {
      const next = await invoke<RendererUpdateStatus>('check_renderer_update')
      startTransition(() => {
        setRendererUpdate(next)
        setRendererUpdateError(null)
      })
    } catch (err) {
      startTransition(() => {
        setRendererUpdate(null)
        setRendererUpdateError(options?.silent ? null : err instanceof Error ? err.message : String(err))
      })
    } finally {
      setCheckingRendererUpdate(false)
    }
  }, [])

  useEffect(() => {
    void refreshState()
    void checkForClientUpdate({ silent: true })
    const unlisteners: Array<() => void> = []
    void listen<AppStatePayload>('app:state', (event) => {
      startTransition(() => setAppState(event.payload))
    }).then((unlisten) => unlisteners.push(unlisten))
    void listen<ClientUpdateProgress>('client-update-progress', (event) => {
      startTransition(() => {
        setClientUpdateProgress({
          ...event.payload,
          percent: clampPercent(event.payload.percent),
        })
      })
    }).then((unlisten) => unlisteners.push(unlisten))
    return () => {
      for (const unlisten of unlisteners) unlisten()
    }
  }, [checkForClientUpdate, refreshState])

  const rendererUpdateAuthenticated = Boolean(appState?.runtime.isAuthenticated)
  const rendererUpdateIsServer = Boolean(appState?.config.isServer)
  const rendererUpdateRegisteredUserId = appState?.config.registeredUserId ?? ''
  const rendererUpdateUserId = appState?.config.userId ?? ''
  const shouldCheckRendererUpdate = Boolean(
    rendererUpdateAuthenticated
    && rendererUpdateIsServer
    && rendererUpdateRegisteredUserId.trim() === rendererUpdateUserId.trim()
  )

  useEffect(() => {
    if (!shouldCheckRendererUpdate) {
      startTransition(() => {
        setRendererUpdate(null)
        setRendererUpdateError(null)
      })
      return
    }
    void checkForRendererUpdate({ silent: true })
  }, [checkForRendererUpdate, shouldCheckRendererUpdate])

  async function runAction(label: string, action: () => Promise<unknown>) {
    const wasAuthenticated = Boolean(appState?.runtime.isAuthenticated)
    setBusy(label)
    setError(null)
    try {
      await action()
      await refreshState()
    } catch (err) {
      let nextState: AppStatePayload | null = null
      try {
        nextState = await refreshState()
      } catch {
        nextState = null
      }
      if (wasAuthenticated && nextState && !nextState.runtime.isAuthenticated) {
        return
      }
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setBusy(null)
    }
  }

  async function installClientUpdate() {
    setBusy('Updating client')
    setError(null)
    setClientUpdateProgress({
      phase: 'starting',
      percent: 1,
      message: 'Preparando actualizacion del cliente...',
      downloadedBytes: null,
      totalBytes: null,
    })
    try {
      await invoke('install_client_update')
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
      setClientUpdateProgress(null)
      setBusy(null)
    }
  }

  async function installRendererUpdate() {
    await runAction('Updating renderer', () => invoke('install_renderer_update'))
    await checkForRendererUpdate()
  }

  async function confirmPendingUpdate() {
    const requestedUpdate = pendingUpdate
    if (!requestedUpdate) return
    if (requestedUpdate.kind === 'client') {
      await installClientUpdate()
      return
    }

    setPendingUpdate(null)
    await installRendererUpdate()
  }

  async function persistSettingsDraft(nextDraft = settingsDraft) {
    const input = settingsInputFromDraft(nextDraft)
    await invoke('save_settings', { input })
    lastSavedSettingsPayload.current = JSON.stringify(input)
  }

  useEffect(() => {
    if (!appState) return
    const input = settingsInputFromDraft(settingsDraft)
    const serialized = JSON.stringify(input)
    if (serialized === lastSavedSettingsPayload.current) return

    const timeout = window.setTimeout(() => {
      if (serialized === lastSavedSettingsPayload.current) return
      const requestId = settingsSaveRequestId.current + 1
      settingsSaveRequestId.current = requestId
      setBusy('Saving settings')
      setError(null)
      void invoke('save_settings', { input })
        .then(() => {
          if (settingsSaveRequestId.current === requestId) {
            lastSavedSettingsPayload.current = serialized
          }
        })
        .catch((err) => {
          if (settingsSaveRequestId.current === requestId) {
            setError(err instanceof Error ? err.message : String(err))
          }
        })
        .finally(() => {
          if (settingsSaveRequestId.current === requestId) {
            setBusy(null)
          }
        })
    }, 650)

    return () => window.clearTimeout(timeout)
  }, [appState, settingsDraft])

  async function startAutoRendererWatcher() {
    await runAction('Starting watcher', async () => {
      if (appState && !canUseAutoRenderer(appState.config)) {
        throw new Error('Auto Renderer requires Miru Plus.')
      }
      let draftToSave = settingsDraft
      const needsStablePath = !appState?.runtime.osuStableDetected
        || !appState.runtime.replayDirReady
        || !appState.runtime.songsDirReady
      if (needsStablePath) {
        const selected = await invoke<string | null>('select_osu_stable_path')
        if (!selected) {
          throw new Error('Select the osu!stable folder to start Auto Renderer.')
        }
        draftToSave = {
          ...settingsDraft,
          autoRenderer: {
            ...settingsDraft.autoRenderer,
            osuStablePathOverride: selected,
          },
        }
        startTransition(() => setSettingsDraft(draftToSave))
      }
      await persistSettingsDraft(draftToSave)
      await invoke('start_watcher')
    })
  }

  async function selectRendererOverridePath() {
    setBusy('Selecting renderer binary')
    setError(null)
    try {
      const selected = await invoke<string | null>('select_renderer_path')
      if (!selected) return
      startTransition(() => {
        setSettingsDraft((current) => ({
          ...current,
          rendererOverridePath: selected,
        }))
      })
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setBusy(null)
    }
  }

  async function selectOsuStableRoot() {
    setBusy('Selecting osu!stable root')
    setError(null)
    try {
      const selected = await invoke<string | null>('select_osu_stable_path')
      if (!selected) return
      const nextDraft = {
        ...settingsDraft,
        autoRenderer: {
          ...settingsDraft.autoRenderer,
          osuStablePathOverride: selected,
        },
      }
      startTransition(() => {
        setSettingsDraft(nextDraft)
      })
      await persistSettingsDraft(nextDraft)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setBusy(null)
    }
  }

  async function prepareBenchmark() {
    await runAction('Preparing benchmark', async () => {
      const availableSlots = await invoke<number>('get_server_slots')
      setSlots(availableSlots)
      const alreadyRegistered = Boolean(appState?.config.isServer || workerStats?.registered)
      if (availableSlots <= 0 && !alreadyRegistered) {
        throw new Error('No public renderer slots are available right now.')
      }
      const plan = await invoke<BenchmarkDownloadPlan>('get_benchmark_download_plan')
      setBenchmarkPlan(plan)
    })
  }

  async function runBenchmark() {
    setBenchmarkPlan(null)
    await runAction('Running benchmark', async () => {
      const result = await invoke<BenchmarkResult>('run_benchmark')
      setLastBenchmark(result)
      const slotsAfterBenchmark = await invoke<number>('get_server_slots')
      setSlots(slotsAfterBenchmark)
      const stats = await invoke<WorkerStatsPayload>('get_worker_stats')
      setWorkerStats(stats)
      setShowRegisterPrompt(!stats.registered && !appState?.config.isServer && slotsAfterBenchmark > 0 && benchmarkPasses(result))
    })
  }

  async function loadSlots() {
    await runAction('Checking slots', async () => {
      const nextSlots = await invoke<number>('get_server_slots')
      setSlots(nextSlots)
    })
  }

  async function addAndConnectRenderer() {
    await runAction('Adding renderer', async () => {
      const currentStats = await invoke<WorkerStatsPayload>('get_worker_stats')
      setWorkerStats(currentStats)
      if (currentStats.registered || appState?.config.isServer) {
        setShowRegisterPrompt(false)
        await persistSettingsDraft({
          ...settingsDraft,
          serverName: normalizeServerName(serverName),
        })
        await invoke('connect_worker')
        const [stats, history] = await Promise.all([
          invoke<WorkerStatsPayload>('get_worker_stats'),
          invoke<WorkerHistoryEntry[]>('get_worker_history'),
        ])
        setWorkerStats(stats)
        setWorkerHistory(history)
        setSlots(stats.slotsAvailable)
        return
      }
      const nextServerName = normalizeServerName(serverName)
      if (!nextServerName) {
        throw new Error('Renderer name is required.')
      }
      const availableSlots = await invoke<number>('get_server_slots')
      setSlots(availableSlots)
      if (availableSlots <= 0 && !appState?.config.isServer) {
        throw new Error('No public renderer slots are available right now.')
      }
      await persistSettingsDraft({ ...settingsDraft, serverName: nextServerName })
      await invoke('register_server', { input: { name: nextServerName } })
      setShowRegisterPrompt(false)
      try {
        await invoke('connect_worker')
      } catch (err) {
        await refreshState()
        throw err
      }
      const [stats, history] = await Promise.all([
        invoke<WorkerStatsPayload>('get_worker_stats'),
        invoke<WorkerHistoryEntry[]>('get_worker_history'),
      ])
      setWorkerStats(stats)
      setWorkerHistory(history)
      setSlots(stats.slotsAvailable)
    })
  }

  const updateServerNameDraft = (value: string) => {
    const nextName = value.slice(0, MAX_SERVER_NAME_LENGTH)
    setServerName(nextName)
    setSettingsDraft((current) => ({
      ...current,
      serverName: nextName,
    }))
  }

  async function connectWorkerWithCurrentName() {
    await runAction('Connecting worker', async () => {
      const nextDraft = {
        ...settingsDraft,
        serverName: normalizeServerName(serverName),
      }
      await persistSettingsDraft(nextDraft)
      await invoke('connect_worker')
      const [stats, history] = await Promise.all([
        invoke<WorkerStatsPayload>('get_worker_stats'),
        invoke<WorkerHistoryEntry[]>('get_worker_history'),
      ])
      setWorkerStats(stats)
      setWorkerHistory(history)
      setSlots(stats.slotsAvailable)
    })
  }

  const runtime = appState?.runtime
  const config = appState?.config
  const loginAction = () => runAction('Opening login', () => invoke('login'))
  const logoutAction = () => runAction('Logging out', () => invoke('logout'))

  return (
    <main className="app-shell">
      <aside className="sidebar">
        <div className="brand-block">
          <img src="/logo-lg.png" alt="Miru" className="brand-logo" />
          <div>
            <p className="eyebrow">desktop client</p>
            <h1>Miru</h1>
          </div>
        </div>

        <nav className="nav-list" aria-label="Miru sections">
          {navItems.map((item) => {
            const Icon = item.icon
            return (
              <button
                key={item.key}
                className={`nav-item ${activeTab === item.key ? 'active' : ''}`}
                type="button"
                onClick={() => setActiveTab(item.key)}
              >
                <Icon size={18} />
                <span>{item.label}</span>
              </button>
            )
          })}
        </nav>

        <div className="sidebar-footer">
          <AccountDock
            authenticated={Boolean(runtime?.isAuthenticated)}
            username={config?.username || ''}
            avatarUrl={config?.userAvatarUrl || ''}
            onLogin={loginAction}
            onLogout={logoutAction}
          />
        </div>
      </aside>

      <section className="workspace">
        <header className="topbar">
          <div>
            <p className="eyebrow">control plane</p>
            <h2>{navItems.find((item) => item.key === activeTab)?.label}</h2>
          </div>
          <div className="topbar-actions">
            {busy && (
              <span className="busy-chip">
                <LoaderCircle size={15} className="spin" />
                {busy}
              </span>
            )}
            <button className="ghost-button" type="button" onClick={() => void refreshState()}>
              Refresh
            </button>
          </div>
        </header>

        {error && (
          <div className="error-banner" role="alert">
            {error}
          </div>
        )}

        {clientUpdate?.updateAvailable && (
          <UpdateBanner
            update={clientUpdate}
            checking={checkingClientUpdate}
            installing={busy === 'Updating client'}
            progress={clientUpdateProgress}
            onRefresh={() => void checkForClientUpdate()}
            onUpdate={() => {
              if (clientUpdate) setPendingUpdate({ kind: 'client', update: clientUpdate })
            }}
          />
        )}

        {!clientUpdate?.updateAvailable && clientUpdateError && (
          <div className="update-banner muted-update" role="status">
            <div>
              <b>Update check unavailable</b>
              <p>{clientUpdateError}</p>
            </div>
            <button className="secondary-button" type="button" onClick={() => void checkForClientUpdate()} disabled={checkingClientUpdate}>
              {checkingClientUpdate ? 'Checking...' : 'Retry'}
            </button>
          </div>
        )}

        {shouldCheckRendererUpdate && rendererUpdate?.updateAvailable && (
          <RendererUpdateBanner
            update={rendererUpdate}
            checking={checkingRendererUpdate}
            installing={busy === 'Updating renderer'}
            onRefresh={() => void checkForRendererUpdate()}
            onUpdate={() => setPendingUpdate({ kind: 'renderer', update: rendererUpdate })}
          />
        )}

        {shouldCheckRendererUpdate && !rendererUpdate?.updateAvailable && rendererUpdateError && (
          <div className="update-banner muted-update" role="status">
            <div>
              <b>Renderer update check unavailable</b>
              <p>{rendererUpdateError}</p>
            </div>
            <button className="secondary-button" type="button" onClick={() => void checkForRendererUpdate()} disabled={checkingRendererUpdate}>
              {checkingRendererUpdate ? 'Checking...' : 'Retry'}
            </button>
          </div>
        )}

        {!appState ? (
          <div className="loading-state">
            <LoaderCircle className="spin" />
            Loading Miru Desktop Client
          </div>
        ) : (
          <motion.div
            key={activeTab}
            initial={{ opacity: 0, y: 12 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ duration: 0.22 }}
          >
            {activeTab === 'dashboard' && (
              <Dashboard
                state={appState}
                onStart={() => startAutoRendererWatcher()}
                onStop={() => runAction('Stopping watcher', () => invoke('stop_watcher'))}
                onBenchmark={() => void prepareBenchmark()}
                lastBenchmark={lastBenchmark}
                canUseAutoRenderer={canUseAutoRenderer(appState.config)}
              />
            )}
            {activeTab === 'auto' && (
              <AutoRenderer
                state={appState}
                draft={settingsDraft}
                setDraft={setSettingsDraft}
                library={autoRendererLibrary}
                libraryError={autoRendererLibraryError}
                onStart={() => startAutoRendererWatcher()}
                onStop={() => runAction('Stopping watcher', () => invoke('stop_watcher'))}
              />
            )}
            {activeTab === 'server' && (
              <ServerWorker
                state={appState}
                serverName={serverName}
                setServerName={updateServerNameDraft}
                slots={slots}
                lastBenchmark={lastBenchmark}
                workerStats={workerStats}
                onLoadSlots={() => void loadSlots()}
                onBenchmark={() => void prepareBenchmark()}
                onConnect={() => connectWorkerWithCurrentName()}
                onDisconnect={() => runAction('Disconnecting worker', () => invoke('disconnect_worker'))}
                onRemove={() => runAction('Removing server', () => invoke('remove_server'))}
              />
            )}
            {activeTab === 'history' && <HistoryPanel entries={appState.history} workerHistory={workerHistory} workerStats={workerStats} />}
            {activeTab === 'settings' && (
              <SettingsPanel
                state={appState}
                workerStats={workerStats}
                draft={settingsDraft}
                setDraft={setSettingsDraft}
                onSelectRendererPath={() => void selectRendererOverridePath()}
                onSelectOsuStableRoot={() => void selectOsuStableRoot()}
                onOpenDiscordInvite={() => runAction('Opening Discord invite', () => invoke('open_discord_invite'))}
              />
            )}
            {activeTab === 'logs' && (
              <LogsPanel logs={appState.logs} onOpen={() => runAction('Opening logs', () => invoke('open_logs_dir'))} />
            )}
          </motion.div>
        )}
      </section>
      {showRegisterPrompt && lastBenchmark && (
        <RegisterPrompt
          benchmark={lastBenchmark}
          serverName={serverName}
          setServerName={updateServerNameDraft}
          slots={slots ?? 0}
          busy={busy === 'Adding renderer'}
          onCancel={() => setShowRegisterPrompt(false)}
          onConfirm={() => void addAndConnectRenderer()}
        />
      )}
      {benchmarkPlan && (
        <BenchmarkDownloadPrompt
          plan={benchmarkPlan}
          busy={busy === 'Running benchmark'}
          onCancel={() => setBenchmarkPlan(null)}
          onConfirm={() => void runBenchmark()}
        />
      )}
      {pendingUpdate && (
        <UpdateConfirmPrompt
          request={pendingUpdate}
          busy={busy === 'Updating client' || busy === 'Updating renderer'}
          clientProgress={pendingUpdate.kind === 'client' ? clientUpdateProgress : null}
          onCancel={() => setPendingUpdate(null)}
          onConfirm={() => void confirmPendingUpdate()}
        />
      )}
    </main>
  )
}

function UpdateBanner(props: {
  update: ClientUpdateStatus
  checking: boolean
  installing: boolean
  progress: ClientUpdateProgress | null
  onRefresh: () => void
  onUpdate: () => void
}) {
  const progressPercent = props.progress ? clampPercent(props.progress.percent) : 0
  return (
    <section className="update-banner" aria-live="polite">
      <div className="update-copy">
        <span className="update-icon" aria-hidden="true"><Download size={18} /></span>
        <div>
          <b>Miru Desktop Client {props.update.latestVersion} is available</b>
          <p>
            Installed {props.update.currentVersion}. Download {props.update.assetName} ({formatBytes(props.update.sizeBytes)}).
          </p>
          {props.progress && (
            <div className="update-inline-progress" role="status">
              <div className="update-progress-meta">
                <span>{props.progress.message}</span>
                <strong>{progressPercent}%</strong>
              </div>
              <div className="progress-track"><span style={{ width: `${progressPercent}%` }} /></div>
            </div>
          )}
        </div>
      </div>
      <div className="update-actions">
        <button className="secondary-button" type="button" onClick={props.onRefresh} disabled={props.checking || props.installing}>
          {props.checking ? 'Checking...' : 'Check again'}
        </button>
        <button className="primary-button" type="button" onClick={props.onUpdate} disabled={props.installing}>
          {props.installing ? <LoaderCircle size={16} className="spin" /> : <Download size={16} />}
          {props.installing ? 'Updating' : 'Update client'}
        </button>
      </div>
    </section>
  )
}

function RendererUpdateBanner(props: {
  update: RendererUpdateStatus
  checking: boolean
  installing: boolean
  onRefresh: () => void
  onUpdate: () => void
}) {
  const currentLabel = props.update.currentVersion === 'unknown'
    ? 'Installed renderer version is unknown'
    : `Installed ${props.update.currentVersion}`

  return (
    <section className="update-banner renderer-update" aria-live="polite">
      <div className="update-copy">
        <span className="update-icon" aria-hidden="true"><Cpu size={18} /></span>
        <div>
          <b>Miru Renderer {props.update.latestVersion} is available</b>
          <p>
            {currentLabel}. Install {props.update.assetName} ({formatBytes(props.update.sizeBytes)}) from the public renderer release.
          </p>
        </div>
      </div>
      <div className="update-actions">
        <button className="secondary-button" type="button" onClick={props.onRefresh} disabled={props.checking || props.installing}>
          {props.checking ? 'Checking...' : 'Check again'}
        </button>
        <button className="primary-button" type="button" onClick={props.onUpdate} disabled={props.installing}>
          {props.installing ? <LoaderCircle size={16} className="spin" /> : <Download size={16} />}
          {props.installing ? 'Updating' : 'Update renderer'}
        </button>
      </div>
    </section>
  )
}

function Dashboard(props: {
  state: AppStatePayload
  lastBenchmark: BenchmarkResult | null
  canUseAutoRenderer: boolean
  onStart: () => Promise<void>
  onStop: () => Promise<void>
  onBenchmark: () => void
}) {
  const { state, lastBenchmark } = props
  const benchmark = lastBenchmark ?? state.runtime.lastBenchmark
  const running = state.runtime.watcherStatus === 'running'
  return (
    <div className="grid-main">
      <Card>
        <Metric icon={MonitorPlay} label="Auto Renderer" value={state.runtime.watcherStatus} tone={running ? 'good' : 'neutral'} />
        {!props.canUseAutoRenderer && <p className="settings-warning compact">Auto Renderer is a Miru Plus feature.</p>}
        <div className="button-row">
          <button className="primary-button" type="button" onClick={() => void props.onStart()} disabled={running || !props.canUseAutoRenderer}>
            <Play size={16} />
            Start
          </button>
          <button className="secondary-button" type="button" onClick={() => void props.onStop()} disabled={!running}>
            <Square size={16} />
            Stop
          </button>
        </div>
      </Card>

      <Card>
        <Metric icon={Cpu} label="Worker" value={state.runtime.workerStatus} tone={state.runtime.workerStatus === 'connected' ? 'good' : 'neutral'} />
        <p className="muted">Renderer: {state.runtime.rendererInstalled ? 'ready' : 'not downloaded'}</p>
        <button className="secondary-button full" type="button" onClick={props.onBenchmark}>
          <Gauge size={16} />
          Run benchmark
        </button>
      </Card>

      <Card>
        <Metric icon={FileClock} label="Latest" value={state.history[0]?.status || 'empty'} tone="neutral" />
        <p className="muted">{state.history[0]?.title || 'No local events yet'}</p>
        {benchmark && <p className="muted">{benchmark.gpuName} | {benchmark.downloadMbps.toFixed(1)} down / {benchmark.uploadMbps.toFixed(1)} up</p>}
      </Card>
    </div>
  )
}

function AutoRenderer(props: {
  state: AppStatePayload
  draft: SettingsDraft
  setDraft: (next: SettingsDraft) => void
  library: AutoRendererLibrary
  libraryError: string | null
  onStart: () => Promise<void>
  onStop: () => Promise<void>
}) {
  const { state, draft, setDraft, library, libraryError, onStart, onStop } = props
  const running = state.runtime.watcherStatus === 'running'
  const runtime = state.runtime
  const autoRenderer = normalizeAutoRendererConfig(draft.autoRenderer)
  const plusEnabled = canUseAutoRenderer(state.config)
  const keyOptions = Array.from({ length: 18 }, (_, index) => index + 1)
  const latestEvent = runtime.lastAutoRendererEvent
  const [openSections, setOpenSections] = useState<Record<string, boolean>>({})
  const presetIds = library.presets.map((preset) => preset.id).join('|')
  const skinIds = library.skins.map((skin) => skin.id).join('|')

  useEffect(() => {
    const currentAutoRenderer = normalizeAutoRendererConfig(draft.autoRenderer)
    const availablePresetIds = new Set(library.presets.map((preset) => preset.id))
    const availableSkinIds = new Set(library.skins.map((skin) => skin.id))
    const presetMissing = currentAutoRenderer.selectedPresetId && !availablePresetIds.has(currentAutoRenderer.selectedPresetId)
    const selectedPreset = library.presets.find((entry) => entry.id === currentAutoRenderer.selectedPresetId)
    const presetSkinId = extractPresetSkinId(selectedPreset?.config)
    const selectedSkinId = presetSkinId && availableSkinIds.has(presetSkinId) ? presetSkinId : 'default'
    if (!presetMissing && currentAutoRenderer.selectedSkinId === selectedSkinId) return

    setDraft({
      ...draft,
      autoRenderer: {
        ...currentAutoRenderer,
        selectedPresetId: presetMissing ? null : currentAutoRenderer.selectedPresetId,
        selectedSkinId,
      },
    })
  }, [draft, library.presets, library.skins, presetIds, setDraft, skinIds])

  const setAutoRenderer = (next: AutoRendererConfig) => {
    setDraft({
      ...draft,
      autoRenderer: next,
    })
  }

  const toggleKeyCount = (value: number) => {
    const selected = autoRenderer.keyCounts.includes(value)
      ? autoRenderer.keyCounts.filter((entry) => entry !== value)
      : [...autoRenderer.keyCounts, value].sort((left, right) => left - right)
    setAutoRenderer({
      ...autoRenderer,
      keyCounts: selected,
    })
  }

  const updateNumericRule = (ruleName: NumericRuleKey, next: Partial<NumericRule>) => {
    setAutoRenderer({
      ...autoRenderer,
      [ruleName]: {
        ...autoRenderer[ruleName],
        ...next,
      },
    })
  }

  const updateJudgmentRule = (ruleName: JudgmentRuleKey, next: Partial<NumericRule>) => {
    setAutoRenderer({
      ...autoRenderer,
      judgmentRules: {
        ...autoRenderer.judgmentRules,
        [ruleName]: {
          ...autoRenderer.judgmentRules[ruleName],
          ...next,
        },
      },
    })
  }

  const selectPreset = (presetId: string) => {
    const selectedPresetId = presetId || null
    const preset = library.presets.find((entry) => entry.id === selectedPresetId)
    const presetSkinId = extractPresetSkinId(preset?.config)
    const availableSkinIds = new Set(library.skins.map((skin) => skin.id))
    setAutoRenderer({
      ...autoRenderer,
      selectedPresetId,
      selectedSkinId: presetSkinId && availableSkinIds.has(presetSkinId) ? presetSkinId : 'default',
    })
  }

  const toggleSection = (section: string) => {
    setOpenSections((current) => ({ ...current, [section]: !current[section] }))
  }

  const setVideoQuality = (resolution: Resolution) => {
    setDraft({
      ...draft,
      resolution,
    })
  }

  return (
    <div className="panel-grid">
      <Card>
        <SectionTitle icon={MonitorPlay} title="Watcher" />
        <div className="status-grid">
          <StatusPill label={`osu!stable ${runtime.osuStableDetected ? 'found' : 'missing'}`} tone={runtime.osuStableDetected ? 'good' : 'warn'} />
          <StatusPill label={`auto replays ${runtime.replayDirReady ? 'ready' : 'missing'}`} tone={runtime.replayDirReady ? 'good' : 'warn'} />
          <StatusPill label={`saved replays ${runtime.stableReplayDirReady ? 'ready' : 'optional'}`} tone={runtime.stableReplayDirReady ? 'good' : 'neutral'} />
          <StatusPill label={`songs ${runtime.songsDirReady ? 'ready' : 'missing'}`} tone={runtime.songsDirReady ? 'good' : 'warn'} />
          <StatusPill label={draft.resolution === 'p1080' ? '1080p60' : '720p60'} tone="neutral" />
          <StatusPill label={plusEnabled ? 'Plus enabled' : 'Plus required'} tone={plusEnabled ? 'good' : 'warn'} />
        </div>
        {!plusEnabled && (
          <p className="settings-warning">
            Auto Renderer is visible to everyone, but configuring filters and starting the watcher requires Miru Plus. {entitlementSummary(state.config)}
          </p>
        )}
        <div className="watcher-select-grid">
          <label className="field-label">
            Video quality
            <select value={draft.resolution} onChange={(event) => setVideoQuality(event.target.value as Resolution)} disabled={!plusEnabled}>
              <option value="p720">720p60</option>
              <option value="p1080">1080p60</option>
            </select>
          </label>
          <label className="field-label">
            Preset
            <select value={autoRenderer.selectedPresetId ?? ''} onChange={(event) => selectPreset(event.target.value)} disabled={!plusEnabled}>
              <option value="">No preset</option>
              {library.presets.map((preset) => (
                <option key={preset.id} value={preset.id}>
                  {preset.name}{preset.isDefault ? ' - default' : ''}
                </option>
              ))}
            </select>
          </label>
        </div>
        <p className="library-message neutral">To change the skin, configure it in the selected preset on the web.</p>
        {libraryError && <p className="library-message">Preset library could not refresh: {libraryError}</p>}
        <div className="watcher-warning">
          If you want to save a FAIL replay, press F2 right after failing. Miru also watches the osu!stable Replays folder.
        </div>
        <div className="result-stack">
          <p className="muted">Install root: {runtime.osuStableRoot || 'Auto-detecting from LOCALAPPDATA\\osu!'}</p>
          <p className="muted">Auto replay dir: {runtime.replayDir || 'Unavailable until osu!stable is resolved'}</p>
          <p className="muted">Saved replay dir: {runtime.stableReplayDir || 'Optional; create osu!\\Replays or save a replay with F2'}</p>
          <p className="muted">Songs dir: {runtime.songsDir || 'Unavailable until osu!stable is resolved'}</p>
        </div>
        <div className="button-row">
          <button className="primary-button" type="button" onClick={() => void onStart()} disabled={running || !plusEnabled}>
            <Play size={16} />
            Start watcher
          </button>
          <button className="secondary-button" type="button" onClick={() => void onStop()} disabled={!running}>
            <Square size={16} />
            Stop
          </button>
        </div>
      </Card>

      <Card>
        <SectionTitle icon={ListFilter} title="Replay filters" />
        <div className="filter-editor">
          <FilterSection
            id="map"
            title="Map properties"
            activeCount={activeCount([autoRenderer.bpmRule, autoRenderer.hpRule, autoRenderer.csRule, autoRenderer.odRule, autoRenderer.durationRule]) + (autoRenderer.keyCounts.length ? 1 : 0)}
            open={Boolean(openSections.map)}
            onToggle={toggleSection}
            disabled={!plusEnabled}
          >
            <div className="rule-card">
              <span className="field-kicker">Key count</span>
              <div className="chip-row">
                {keyOptions.map((keyCount) => (
                  <button
                    key={keyCount}
                    className={`chip-button ${autoRenderer.keyCounts.includes(keyCount) ? 'active' : ''}`}
                    type="button"
                    onClick={() => toggleKeyCount(keyCount)}
                    disabled={!plusEnabled}
                  >
                    {keyCount}K
                  </button>
                ))}
              </div>
              <p className="muted">
                {autoRenderer.keyCounts.length
                  ? `Only ${autoRenderer.keyCounts.join(', ')}K maps will match.`
                  : 'No key restriction. Every mania keycount is eligible.'}
              </p>
            </div>
            <NumericRuleEditor label="BPM" rule={autoRenderer.bpmRule} onChange={(next) => updateNumericRule('bpmRule', next)} step={0.01} disabled={!plusEnabled} />
            <NumericRuleEditor label="HP" rule={autoRenderer.hpRule} onChange={(next) => updateNumericRule('hpRule', next)} step={0.1} disabled={!plusEnabled} />
            <NumericRuleEditor label="CS / Keys" rule={autoRenderer.csRule} onChange={(next) => updateNumericRule('csRule', next)} step={0.1} disabled={!plusEnabled} />
            <NumericRuleEditor label="OD" rule={autoRenderer.odRule} onChange={(next) => updateNumericRule('odRule', next)} step={0.1} disabled={!plusEnabled} />
            <NumericRuleEditor label="Duration seconds" rule={autoRenderer.durationRule} onChange={(next) => updateNumericRule('durationRule', next)} step={1} disabled={!plusEnabled} />
          </FilterSection>

          <FilterSection
            id="notes"
            title="Note counts"
            activeCount={activeCount([autoRenderer.longNoteRule, autoRenderer.normalNoteRule, autoRenderer.totalNoteRule])}
            open={Boolean(openSections.notes)}
            onToggle={toggleSection}
            disabled={!plusEnabled}
          >
            <NumericRuleEditor label="Long note count" rule={autoRenderer.longNoteRule} onChange={(next) => updateNumericRule('longNoteRule', next)} disabled={!plusEnabled} />
            <NumericRuleEditor label="Normal note count" rule={autoRenderer.normalNoteRule} onChange={(next) => updateNumericRule('normalNoteRule', next)} disabled={!plusEnabled} />
            <NumericRuleEditor label="Total note count" rule={autoRenderer.totalNoteRule} onChange={(next) => updateNumericRule('totalNoteRule', next)} disabled={!plusEnabled} />
          </FilterSection>

          <FilterSection
            id="replay"
            title="Replay result"
            activeCount={activeCount([autoRenderer.maxComboRule, autoRenderer.accuracyRule, autoRenderer.ppRule])}
            open={Boolean(openSections.replay)}
            onToggle={toggleSection}
            disabled={!plusEnabled}
          >
            <NumericRuleEditor label="Max combo" rule={autoRenderer.maxComboRule} onChange={(next) => updateNumericRule('maxComboRule', next)} disabled={!plusEnabled} />
            <NumericRuleEditor label="Accuracy %" rule={autoRenderer.accuracyRule} onChange={(next) => updateNumericRule('accuracyRule', next)} step={0.01} disabled={!plusEnabled} />
            <NumericRuleEditor label="PP" rule={autoRenderer.ppRule} onChange={(next) => updateNumericRule('ppRule', next)} step={0.01} disabled={!plusEnabled} />
          </FilterSection>

          <FilterSection
            id="judgments"
            title="Judgments"
            activeCount={activeCount(Object.values(autoRenderer.judgmentRules))}
            open={Boolean(openSections.judgments)}
            onToggle={toggleSection}
            disabled={!plusEnabled}
          >
            <NumericRuleEditor label="MAX / 320 count" rule={autoRenderer.judgmentRules.max} onChange={(next) => updateJudgmentRule('max', next)} disabled={!plusEnabled} />
            <NumericRuleEditor label="300 count" rule={autoRenderer.judgmentRules.n300} onChange={(next) => updateJudgmentRule('n300', next)} disabled={!plusEnabled} />
            <NumericRuleEditor label="200 count" rule={autoRenderer.judgmentRules.n200} onChange={(next) => updateJudgmentRule('n200', next)} disabled={!plusEnabled} />
            <NumericRuleEditor label="100 count" rule={autoRenderer.judgmentRules.n100} onChange={(next) => updateJudgmentRule('n100', next)} disabled={!plusEnabled} />
            <NumericRuleEditor label="50 count" rule={autoRenderer.judgmentRules.n50} onChange={(next) => updateJudgmentRule('n50', next)} disabled={!plusEnabled} />
            <NumericRuleEditor label="Miss count" rule={autoRenderer.judgmentRules.miss} onChange={(next) => updateJudgmentRule('miss', next)} disabled={!plusEnabled} />
          </FilterSection>
        </div>
      </Card>

      <Card className="wide-card">
        <SectionTitle icon={FileClock} title="Latest replay event" />
        {latestEvent ? (
          <div className="timeline-row compact auto-event-row">
            <span>{latestEvent.replayName}</span>
            <div>
              <b>{latestEvent.title}</b>
              <p>{latestEvent.detail}</p>
            </div>
            <StatusPill label={latestEvent.status} tone={latestEvent.status === 'started' ? 'good' : latestEvent.status === 'failed' ? 'bad' : 'neutral'} />
          </div>
        ) : (
          <p className="muted">No new stable replays have been processed in this session yet.</p>
        )}
      </Card>
    </div>
  )
}

function ServerWorker(props: {
  state: AppStatePayload
  serverName: string
  setServerName: (value: string) => void
  slots: number | null
  lastBenchmark: BenchmarkResult | null
  workerStats: WorkerStatsPayload | null
  onLoadSlots: () => void
  onBenchmark: () => void
  onConnect: () => Promise<void>
  onDisconnect: () => Promise<void>
  onRemove: () => Promise<void>
}) {
  const benchmark = props.lastBenchmark ?? props.state.runtime.lastBenchmark
  const compliance = props.workerStats?.compliance
  const activeHours = compliance ? (compliance.activeSecondsThisWeek / 3600).toFixed(1) : '0.0'
  const requiredHours = compliance ? (compliance.requiredSecondsPerWeek / 3600).toFixed(0) : '30'
  const benchmarkEligible = benchmark ? benchmarkPasses(benchmark) : false
  const rendererRegistered = Boolean(props.workerStats?.registered || props.state.config.isServer)
  const benchmarkIssuesList = benchmark ? benchmarkIssues(benchmark) : []
  const activeBenchmark = props.state.runtime.benchmark
  const showBenchmarkProgress = Boolean(activeBenchmark && (activeBenchmark.phase !== 'done' || !benchmark))
  const authenticated = props.state.runtime.isAuthenticated
  return (
    <div className="panel-grid">
      <Card>
        <SectionTitle icon={RadioTower} title="Public worker" />
        <div className="status-grid">
          <StatusPill label={props.state.runtime.workerStatus} tone={props.state.runtime.workerStatus === 'connected' ? 'good' : 'neutral'} />
          <StatusPill label={rendererRegistered ? 'registered' : 'not registered'} tone={rendererRegistered ? 'good' : 'warn'} />
          <StatusPill label={props.slots === null ? 'slots unknown' : `${props.slots}/${props.workerStats?.slotsTotal ?? 5} slots`} tone={(props.slots ?? 0) > 0 ? 'good' : 'warn'} />
          <StatusPill label={`${activeHours}/${requiredHours}h weekly`} tone={complianceTone(compliance?.status)} />
        </div>
        <label className="field-label">
          Renderer name
          <input
            value={props.serverName}
            onChange={(event) => props.setServerName(event.target.value)}
            maxLength={MAX_SERVER_NAME_LENGTH}
          />
        </label>
        <div className="button-row">
          <button className="secondary-button" type="button" onClick={props.onLoadSlots}>Check slots</button>
          <button className="secondary-button" type="button" onClick={props.onBenchmark} disabled={!authenticated || (props.slots === 0 && !rendererRegistered)}>Benchmark</button>
        </div>
        <div className="button-row">
          <button className="secondary-button" type="button" onClick={() => void props.onConnect()} disabled={!authenticated}>Connect</button>
          <button className="secondary-button" type="button" onClick={() => void props.onDisconnect()} disabled={!authenticated}>Disconnect</button>
          <button className="danger-button" type="button" onClick={() => void props.onRemove()} disabled={!authenticated}>Remove</button>
        </div>
      </Card>

      <Card>
        <SectionTitle icon={Gauge} title="Benchmark result" />
        {showBenchmarkProgress && activeBenchmark ? (
          <div>
            <div className="progress-track"><span style={{ width: `${activeBenchmark.percent}%` }} /></div>
            <p className="muted">{activeBenchmark.message}</p>
          </div>
        ) : benchmark ? (
          <BenchmarkResultDetails
            benchmark={benchmark}
            eligible={benchmarkEligible}
            registered={rendererRegistered}
            issues={benchmarkIssuesList}
          />
        ) : (
          <p className="muted">Run benchmark to validate this PC before contributing render capacity.</p>
        )}
      </Card>
    </div>
  )
}

function BenchmarkResultDetails(props: {
  benchmark: BenchmarkResult
  eligible: boolean
  registered: boolean
  issues: string[]
}) {
  const renderOk = props.benchmark.renderTimeMs <= props.benchmark.maxRenderMs
  const downloadOk = props.benchmark.downloadMbps >= props.benchmark.minMbps
  const uploadOk = props.benchmark.uploadMbps >= props.benchmark.minUploadMbps
  return (
    <div className="benchmark-result">
      <div className="benchmark-result-head">
        <StatusPill label={props.eligible ? 'benchmark passed' : 'requirements not met'} tone={props.eligible ? 'good' : 'warn'} />
        {props.registered && <StatusPill label="already registered" tone="good" />}
      </div>
      <div className="benchmark-result-grid">
        <ResultMetric label="Render obtained" value={`${(props.benchmark.renderTimeMs / 1000).toFixed(1)}s`} detail={`limit ${(props.benchmark.maxRenderMs / 1000).toFixed(0)}s`} ok={renderOk} />
        <ResultMetric label="Download obtained" value={`${props.benchmark.downloadMbps.toFixed(1)} Mbps`} detail={`min ${props.benchmark.minMbps.toFixed(0)} Mbps`} ok={downloadOk} />
        <ResultMetric label="Upload obtained" value={`${props.benchmark.uploadMbps.toFixed(1)} Mbps`} detail={`min ${props.benchmark.minUploadMbps.toFixed(0)} Mbps`} ok={uploadOk} />
        <ResultMetric label="Latency" value={`${props.benchmark.latencyMs}ms`} detail={props.benchmark.benchmarkSource} ok />
      </div>
      <div className="result-stack">
        <span className="result-gpu">{props.benchmark.gpuName}</span>
        {props.issues.length > 0 ? (
          <p className="muted">{props.issues.join(' ')}</p>
        ) : (
          <p className="muted">This benchmark meets the current renderer requirements.</p>
        )}
      </div>
    </div>
  )
}

function ResultMetric(props: { label: string; value: string; detail: string; ok: boolean }) {
  return (
    <div className={`result-metric ${props.ok ? 'ok' : 'warn'}`}>
      <span>{props.label}</span>
      <b>{props.value}</b>
      <small>{props.detail}</small>
    </div>
  )
}

function FilterSection(props: {
  id: string
  title: string
  activeCount: number
  open: boolean
  onToggle: (id: string) => void
  disabled?: boolean
  children: ReactNode
}) {
  const Icon = props.open ? ChevronDown : ChevronRight
  return (
    <section className="filter-section">
      <button className="filter-section-header" type="button" onClick={() => props.onToggle(props.id)} disabled={props.disabled}>
        <Icon size={17} />
        <span>{props.title}</span>
        <StatusPill label={`${props.activeCount} active`} tone={props.activeCount ? 'good' : 'neutral'} />
      </button>
      {props.open && <div className="filter-section-body">{props.children}</div>}
    </section>
  )
}

function NumericRuleEditor(props: {
  label: string
  rule: NumericRule
  onChange: (next: Partial<NumericRule>) => void
  step?: number
  disabled?: boolean
}) {
  const opLabel =
    props.rule.op === 'eq'
      ? 'exactly'
      : props.rule.op === 'gte'
        ? 'at least'
        : props.rule.op === 'lte'
          ? 'at most'
          : 'between'
  const maxValue = props.rule.maxValue ?? props.rule.value
  return (
    <div className={`rule-card ${props.rule.enabled ? 'enabled' : ''}`}>
      <label className="switch-row compact inline-rule-toggle">
        <input
          type="checkbox"
          checked={props.rule.enabled}
          onChange={(event) => props.onChange({ enabled: event.target.checked })}
          disabled={props.disabled}
        />
        <span>{props.label}</span>
      </label>
      <div className="rule-controls">
        <select
          value={props.rule.op}
          onChange={(event) => props.onChange({ op: event.target.value as NumericRuleOp })}
          disabled={props.disabled || !props.rule.enabled}
        >
          <option value="eq">= exactly</option>
          <option value="gte">&gt;= at least</option>
          <option value="lte">&lt;= at most</option>
          <option value="between">between</option>
        </select>
        <input
          type="number"
          min={0}
          step={props.step ?? 1}
          value={props.rule.value}
          onChange={(event) => props.onChange({ value: Number(event.target.value) || 0 })}
          disabled={props.disabled || !props.rule.enabled}
        />
        {props.rule.op === 'between' && (
          <input
            type="number"
            min={0}
            step={props.step ?? 1}
            value={maxValue}
            onChange={(event) => props.onChange({ maxValue: Number(event.target.value) || 0 })}
            disabled={props.disabled || !props.rule.enabled}
          />
        )}
      </div>
      <p className="muted">
        {props.rule.enabled
          ? props.rule.op === 'between'
            ? `${props.label} must be between ${props.rule.value} and ${maxValue}.`
            : `${props.label} must be ${opLabel} ${props.rule.value}.`
          : `${props.label} is not restricting replay matching.`}
      </p>
    </div>
  )
}

function activeCount(rules: NumericRule[]) {
  return rules.reduce((count, rule) => count + (rule.enabled ? 1 : 0), 0)
}

function extractPresetSkinId(config: unknown): string | null {
  if (!config || typeof config !== 'object' || Array.isArray(config)) return null
  const renderConfig = (config as { renderConfig?: unknown }).renderConfig
  if (!renderConfig || typeof renderConfig !== 'object' || Array.isArray(renderConfig)) return null
  const skinId = (renderConfig as { skinId?: unknown }).skinId
  return typeof skinId === 'string' && skinId.trim() ? skinId.trim() : null
}

function HistoryPanel({ entries, workerHistory, workerStats }: { entries: HistoryEntry[]; workerHistory: WorkerHistoryEntry[]; workerStats: WorkerStatsPayload | null }) {
  const activeHours = workerStats?.compliance ? (workerStats.compliance.activeSecondsThisWeek / 3600).toFixed(1) : '0.0'
  const totalHours = workerStats ? (workerStats.totalRenderTimeSeconds / 3600).toFixed(1) : '0.0'
  return (
    <div className="panel-grid">
      <Card>
        <SectionTitle icon={History} title="Worker replay history" />
        <div className="stats-strip">
          <StatusPill label={`${activeHours}h active this week`} tone={complianceTone(workerStats?.compliance?.status)} />
          <StatusPill label={`${workerStats?.jobsCompleted ?? 0} completed`} tone="good" />
          <StatusPill label={`${workerStats?.jobsFailed ?? 0} failed`} tone={(workerStats?.jobsFailed ?? 0) > 0 ? 'warn' : 'neutral'} />
          <StatusPill label={`${totalHours}h render time`} tone="neutral" />
        </div>
        <div className="timeline">
          {workerHistory.length === 0 && <p className="muted">No worker jobs rendered by this PC yet.</p>}
          {workerHistory.map((entry) => (
            <article key={entry.id} className="timeline-row">
              <span>{formatMaybeDate(entry.completedAt ?? entry.startedAt ?? entry.queuedAt)}</span>
              <div>
                <b>{entry.title}</b>
                <p>{entry.difficulty || entry.replayName || entry.id} {entry.durationMs ? `| ${(entry.durationMs / 1000).toFixed(0)}s` : ''}</p>
              </div>
              <StatusPill label={entry.status.toLowerCase()} tone={entry.status === 'FAILED' ? 'bad' : 'neutral'} />
            </article>
          ))}
        </div>
      </Card>

      <Card>
        <SectionTitle icon={FileClock} title="Local app events" />
        <div className="timeline">
          {entries.length === 0 && <p className="muted">No local events yet.</p>}
          {entries.map((entry) => (
            <article key={entry.id} className="timeline-row compact">
              <span>{new Date(entry.timestamp).toLocaleString()}</span>
              <div>
                <b>{entry.title}</b>
                <p>{entry.detail}</p>
              </div>
              <StatusPill label={entry.status} tone={entry.status === 'failed' ? 'bad' : 'neutral'} />
            </article>
          ))}
        </div>
      </Card>
    </div>
  )
}

function SettingsPanel(props: {
  state: AppStatePayload
  workerStats: WorkerStatsPayload | null
  draft: SettingsDraft
  setDraft: (next: SettingsDraft) => void
  onSelectRendererPath: () => void
  onSelectOsuStableRoot: () => void
  onOpenDiscordInvite: () => void
}) {
  const { draft, setDraft } = props
  const isServerWorker = Boolean(props.workerStats?.registered || props.state.config.isServer)
  return (
    <div className="settings-grid">
      <Card className="settings-card">
        <SectionTitle icon={Cpu} title="Renderer and startup" />
        <div className="settings-stack">
          <label className="field-label settings-path-field">
            Renderer override path
            <div className="path-input-row">
              <input
                value={draft.rendererOverridePath}
                onChange={(event) => setDraft({ ...draft, rendererOverridePath: event.target.value })}
                placeholder="Optional path for development builds"
              />
              <button className="secondary-button path-picker-button" type="button" onClick={props.onSelectRendererPath}>
                Select
              </button>
            </div>
          </label>
          <label className="field-label settings-path-field">
            osu!stable root override
            <div className="path-input-row">
              <input
                value={draft.autoRenderer.osuStablePathOverride}
                onChange={(event) =>
                  setDraft({
                    ...draft,
                    autoRenderer: {
                      ...draft.autoRenderer,
                      osuStablePathOverride: event.target.value,
                    },
                  })
                }
                placeholder={props.state.runtime.osuStableRoot || 'Leave empty to use LOCALAPPDATA\\osu!'}
              />
              <button className="secondary-button path-picker-button" type="button" onClick={props.onSelectOsuStableRoot}>
                Select
              </button>
            </div>
          </label>
          <div className="tool-paths" aria-label="FFmpeg paths">
            <label className="field-label settings-path-field">
              FFmpeg tools directory
              <input value={props.state.runtime.ffmpegTools.directory} readOnly />
            </label>
            <label className="field-label settings-path-field">
              ffmpeg.exe
              <div className="path-input-row tool-path-row">
                <input value={props.state.runtime.ffmpegTools.ffmpeg.path} readOnly />
                <StatusPill label={props.state.runtime.ffmpegTools.ffmpeg.exists ? 'Exists' : 'Missing'} tone={props.state.runtime.ffmpegTools.ffmpeg.exists ? 'good' : 'warn'} />
              </div>
            </label>
            <label className="field-label settings-path-field">
              ffprobe.exe
              <div className="path-input-row tool-path-row">
                <input value={props.state.runtime.ffmpegTools.ffprobe.path} readOnly />
                <StatusPill label={props.state.runtime.ffmpegTools.ffprobe.exists ? 'Exists' : 'Missing'} tone={props.state.runtime.ffmpegTools.ffprobe.exists ? 'good' : 'warn'} />
              </div>
            </label>
          </div>
          <div className="settings-toggle-grid">
            <label className="switch-row compact settings-toggle"><input type="checkbox" checked={draft.autostart} onChange={(event) => setDraft({ ...draft, autostart: event.target.checked })} /> Start Miru with Windows</label>
            <label className="switch-row compact settings-toggle"><input type="checkbox" checked={draft.startMinimizedToTray} onChange={(event) => setDraft({ ...draft, startMinimizedToTray: event.target.checked })} /> Start minimized when Windows starts</label>
            <label className="switch-row compact settings-toggle"><input type="checkbox" checked={draft.closeToTrayOnExit} onChange={(event) => setDraft({ ...draft, closeToTrayOnExit: event.target.checked })} /> Keep running in tray when closing the window</label>
          </div>
          <p className="settings-note">
            Leave the stable path blank to auto-detect the default Windows install. Minimized startup only applies when Miru is launched by Windows autostart, and the close option keeps Miru running in the tray instead of fully exiting.
          </p>
        </div>
      </Card>
      {isServerWorker && (
        <Card className="settings-card">
          <SectionTitle icon={RadioTower} title="Server worker" />
          <div className="settings-stack">
            <div className="settings-toggle-grid">
              <label className="switch-row compact settings-toggle">
                <input type="checkbox" checked={draft.connectWorkerOnLaunch} onChange={(event) => setDraft({ ...draft, connectWorkerOnLaunch: event.target.checked })} />
                Connect as renderer when Miru opens
              </label>
              <label className="switch-row compact settings-toggle">
                <input type="checkbox" checked={draft.showDiscordRendererRole} onChange={(event) => setDraft({ ...draft, showDiscordRendererRole: event.target.checked })} />
                Show renderer role in Discord
              </label>
              <label className="switch-row compact settings-toggle">
                <input type="checkbox" checked={draft.showGpuInStatusImage} onChange={(event) => setDraft({ ...draft, showGpuInStatusImage: event.target.checked })} />
                Show graphics card in Miru status
              </label>
            </div>
            <p className="settings-warning">
              You need to link your Discord account and{' '}
              <button className="inline-link" type="button" onClick={props.onOpenDiscordInvite}>
                join the server
              </button>{' '}
              before Miru can apply the renderer role.
            </p>
            {!props.state.config.discordLinked && (
              <p className="settings-warning">
                Discord is not linked on this backend account, so Miru cannot apply the renderer role yet.
              </p>
            )}
          </div>
        </Card>
      )}
    </div>
  )
}

function LogsPanel({ logs, onOpen }: { logs: string[]; onOpen: () => Promise<void> }) {
  return (
    <Card>
      <div className="logs-header">
        <SectionTitle icon={Terminal} title="Runtime logs" />
        <button className="secondary-button" type="button" onClick={() => void onOpen()}>Open logs folder</button>
      </div>
      <pre className="log-view">{logs.length ? logs.join('\n') : 'No logs yet.'}</pre>
    </Card>
  )
}

function RegisterPrompt(props: {
  benchmark: BenchmarkResult
  serverName: string
  setServerName: (value: string) => void
  slots: number
  busy: boolean
  onCancel: () => void
  onConfirm: () => void
}) {
  return (
    <div className="modal-scrim" role="presentation">
      <section className="confirm-panel" role="dialog" aria-modal="true" aria-labelledby="register-renderer-title">
        <p className="eyebrow">renderer enrollment</p>
        <h3 id="register-renderer-title">Add this PC as a Miru renderer?</h3>
        <p className="muted">
          There are {props.slots} public slots available. Adding this PC grants Plus through the renderer program while it stays registered and meets the weekly active-hours policy.
        </p>
        <div className="benchmark-grid">
          <Metric icon={Gauge} label="Render" value={`${(props.benchmark.renderTimeMs / 1000).toFixed(1)}s`} tone="neutral" />
          <Metric icon={RadioTower} label="Download" value={`${props.benchmark.downloadMbps.toFixed(1)} Mbps`} tone="good" />
          <Metric icon={RadioTower} label="Upload" value={`${props.benchmark.uploadMbps.toFixed(1)} Mbps`} tone="good" />
          <Metric icon={Cpu} label="GPU" value={props.benchmark.gpuName} tone="neutral" />
        </div>
        <label className="field-label">
          Renderer name
          <input
            value={props.serverName}
            onChange={(event) => props.setServerName(event.target.value)}
            maxLength={MAX_SERVER_NAME_LENGTH}
            autoFocus
          />
        </label>
        <div className="button-row end">
          <button className="secondary-button" type="button" onClick={props.onCancel} disabled={props.busy}>Not now</button>
          <button className="primary-button" type="button" onClick={props.onConfirm} disabled={props.busy || props.slots <= 0 || !props.serverName.trim()}>
            {props.busy ? 'Adding...' : 'Add and connect'}
          </button>
        </div>
      </section>
    </div>
  )
}

function BenchmarkDownloadPrompt(props: {
  plan: BenchmarkDownloadPlan
  busy: boolean
  onCancel: () => void
  onConfirm: () => void
}) {
  const pendingItems = props.plan.items
    .map((item) => ({ item, status: downloadPlanStatus(item) }))
    .filter(({ status }) => status !== 'local')
  const hasMissingRequirement = pendingItems.some(({ status }) => status === 'missing')

  return (
    <div className="modal-scrim" role="presentation">
      <section className="confirm-panel" role="dialog" aria-modal="true" aria-labelledby="benchmark-download-title">
        <p className="eyebrow">benchmark setup</p>
        <h3 id="benchmark-download-title">Download Miru and required components?</h3>
        <p className="muted">
          Miru will use the renderer at {props.plan.installPath}. {formatBenchmarkPlanTotal(props.plan)}.
        </p>
        <div className="download-plan-list">
          {pendingItems.map(({ item, status }) => (
            <div className={`download-plan-row ${status}`} key={`${item.name}-${item.detail}`}>
              <span className={`download-plan-icon ${status}`}>
                <Download size={16} />
              </span>
              <div>
                <b>{item.name}</b>
                <p>{item.detail}</p>
              </div>
              <div className="download-plan-meta">
                <strong>{formatBytes(item.sizeBytes)}</strong>
                <small>{formatDownloadPlanStatus(status)}</small>
              </div>
            </div>
          ))}
        </div>
        <div className="button-row end modal-actions">
          <button className="secondary-button" type="button" onClick={props.onCancel} disabled={props.busy}>Cancel</button>
          <button className="primary-button" type="button" onClick={props.onConfirm} disabled={props.busy || hasMissingRequirement}>
            {!props.busy && !hasMissingRequirement && <Download size={16} />}
            {props.busy ? 'Starting...' : hasMissingRequirement ? 'Missing requirement' : 'Download all'}
          </button>
        </div>
      </section>
    </div>
  )
}

function UpdateConfirmPrompt(props: {
  request: PendingUpdateRequest
  busy: boolean
  clientProgress?: ClientUpdateProgress | null
  onCancel: () => void
  onConfirm: () => void
}) {
  const { request } = props
  const isClient = request.kind === 'client'
  const update = request.update
  const title = isClient
    ? `Miru Desktop Client ${update.latestVersion}`
    : `Miru Renderer ${update.latestVersion}`
  const current = isClient
    ? `Installed ${update.currentVersion}`
    : update.currentVersion === 'unknown'
      ? 'Installed renderer version is unknown'
      : `Installed ${update.currentVersion}`
  const detail = request.kind === 'client'
    ? 'Miru se cerrara y el instalador se ejecutara automaticamente.'
    : `El binario se reemplazara automaticamente en ${request.update.installPath}.`
  const clientProgressPercent = props.clientProgress ? clampPercent(props.clientProgress.percent) : 0

  return (
    <div className="modal-scrim" role="presentation">
      <section className="confirm-panel update-confirm-panel" role="dialog" aria-modal="true" aria-labelledby="update-confirm-title">
        <p className="eyebrow">{isClient ? 'client update' : 'renderer update'}</p>
        <h3 id="update-confirm-title">Quieres descargar la nueva actualizacion?</h3>
        <p className="muted">
          {current}. Se descargara y validara {title} antes de instalar. {detail}
        </p>
        <div className="download-plan-list update-confirm-list">
          <div className="download-plan-row download">
            <span className="download-plan-icon download">
              {isClient ? <Download size={16} /> : <Cpu size={16} />}
            </span>
            <div>
              <b>{update.assetName}</b>
              <p>{isClient ? 'Instalador del cliente' : 'Binario del renderer'}</p>
            </div>
            <div className="download-plan-meta">
              <strong>{formatBytes(update.sizeBytes)}</strong>
              <small>Download</small>
            </div>
          </div>
        </div>
        {isClient && props.clientProgress && (
          <div className="update-progress-card" role="status" aria-live="polite">
            <div className="update-progress-meta">
              <span>{props.clientProgress.message}</span>
              <strong>{clientProgressPercent}%</strong>
            </div>
            <div className="progress-track"><span style={{ width: `${clientProgressPercent}%` }} /></div>
            {props.clientProgress.downloadedBytes !== null
              && props.clientProgress.downloadedBytes !== undefined
              && props.clientProgress.totalBytes ? (
                <p className="muted">
                  {formatBytes(props.clientProgress.downloadedBytes)} / {formatBytes(props.clientProgress.totalBytes)}
                </p>
              ) : (
                <p className="muted">La app se cerrara automaticamente cuando el instalador este listo.</p>
              )}
          </div>
        )}
        <div className="button-row end modal-actions">
          <button className="secondary-button" type="button" onClick={props.onCancel} disabled={props.busy}>Cancelar</button>
          <button className="primary-button" type="button" onClick={props.onConfirm} disabled={props.busy}>
            {props.busy ? <LoaderCircle size={16} className="spin" /> : <Download size={16} />}
            {props.busy ? 'Preparando...' : 'Descargar e instalar'}
          </button>
        </div>
      </section>
    </div>
  )
}

function Card({ children, className = '' }: { children: ReactNode; className?: string }) {
  return <section className={`card ${className}`}>{children}</section>
}

function SectionTitle({ icon: Icon, title }: { icon: typeof Activity; title: string }) {
  return <div className="section-title"><Icon size={18} /><h3>{title}</h3></div>
}

function AccountDock(props: {
  authenticated: boolean
  username: string
  avatarUrl: string
  onLogin: () => Promise<void>
  onLogout: () => Promise<void>
}) {
  const username = normalizeDisplayUsername(props.username)
  const label = props.authenticated ? username || 'Signed in' : 'Not logged in'
  const showAvatar = props.authenticated && props.avatarUrl
  return (
    <section className={`account-dock ${props.authenticated ? 'authenticated' : ''}`} aria-label="Account">
      <div className="account-avatar" aria-hidden="true">
        {showAvatar ? (
          <img src={props.avatarUrl} alt="" referrerPolicy="no-referrer" />
        ) : (
          <UserRound size={20} />
        )}
      </div>
      <div className="account-copy">
        <span>{props.authenticated ? 'Account' : 'Session'}</span>
        <b>{label}</b>
      </div>
      {props.authenticated ? (
        <button className="icon-button" type="button" onClick={() => void props.onLogout()} aria-label="Log out" title="Log out">
          <LogOut size={17} />
        </button>
      ) : (
        <button className="account-login-button" type="button" onClick={() => void props.onLogin()}>
          <LogIn size={16} />
          Login
        </button>
      )}
    </section>
  )
}

function Metric({ icon: Icon, label, value, tone }: { icon: typeof Activity; label: string; value: string; tone: 'good' | 'neutral' }) {
  return (
    <div className="metric">
      <div className="metric-head">
        <span className="metric-icon" aria-hidden="true"><Icon size={16} /></span>
        <span>{label}</span>
      </div>
      <b className={tone}>{value}</b>
    </div>
  )
}

function StatusPill({ label, tone }: { label: string; tone: 'good' | 'warn' | 'bad' | 'neutral' }) {
  return <span className={`status-pill ${tone}`}>{label}</span>
}

function complianceTone(status?: string | null): 'good' | 'warn' | 'bad' | 'neutral' {
  if (status === 'ok' || status === 'grace') return 'good'
  if (status === 'behind') return 'warn'
  if (status === 'inactive') return 'bad'
  return 'neutral'
}

function benchmarkPasses(result: BenchmarkResult) {
  return result.downloadMbps >= result.minMbps
    && result.uploadMbps >= result.minUploadMbps
    && result.renderTimeMs <= result.maxRenderMs
}

function benchmarkIssues(result: BenchmarkResult) {
  const issues: string[] = []
  if (result.downloadMbps < result.minMbps) {
    issues.push(`Download is below ${result.minMbps.toFixed(0)} Mbps.`)
  }
  if (result.uploadMbps < result.minUploadMbps) {
    issues.push(`Upload is below ${result.minUploadMbps.toFixed(0)} Mbps.`)
  }
  if (result.renderTimeMs > result.maxRenderMs) {
    issues.push(`Render time is above ${(result.maxRenderMs / 1000).toFixed(0)}s.`)
  }
  return issues
}

function formatMaybeDate(value?: string | null) {
  return value ? new Date(value).toLocaleString() : 'Pending'
}

function downloadPlanStatus(item: DownloadPlanItem) {
  return item.status?.trim() || (item.willDownload ? 'download' : 'local')
}

function formatDownloadPlanStatus(status: string) {
  switch (status) {
    case 'stream':
      return 'Stream'
    case 'generated':
      return 'Generated'
    case 'missing':
      return 'Missing'
    case 'download':
      return 'Download'
    case 'local':
      return 'Local'
    default:
      return status
  }
}

function formatBenchmarkPlanTotal(plan: BenchmarkDownloadPlan) {
  const hasUnknownDownloadSize = plan.items.some((item) => (
    item.willDownload && (item.sizeBytes === undefined || item.sizeBytes === null || !Number.isFinite(item.sizeBytes))
  ))
  const label = hasUnknownDownloadSize ? 'Known download this run' : 'Total download this run'
  const suffix = hasUnknownDownloadSize ? ' + unknown file size' : ''
  return `${label}: ${formatBytes(plan.totalDownloadBytes)}${suffix}`
}

function formatBytes(value?: number | null) {
  if (value === undefined || value === null || !Number.isFinite(value)) {
    return 'Unknown'
  }
  if (value < 1024) {
    return `${value} B`
  }
  const units = ['KiB', 'MiB', 'GiB']
  let size = value / 1024
  let unitIndex = 0
  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024
    unitIndex += 1
  }
  return `${size.toFixed(size >= 10 ? 1 : 2)} ${units[unitIndex]}`
}

export default App
