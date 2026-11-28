// Rust バックエンド（src-tauri/src/）との IPC 契約および Tauri グローバル API の型定義。
// 実行時には読み込まれず、tsc --noEmit（npm run typecheck）での静的型チェックのみに使う。

export type MediaKind = "image" | "video" | "audio";
export type WindowKindKey = MediaKind | "book";

export interface Settings {
  backgroundColor: string;
  backgroundOpacity: number;
  windowCorners: string;
  showFilename: boolean;
  windowSizeMode: string;
  confirmDelete: boolean;
  wheelSensitivity: number;
  imageRendering: string;
  startupZoom: string;
  videoAutoplay: boolean;
  videoEnd: string;
  audioAutoplay: boolean;
  audioEnd: string;
  videoVolume: number;
  audioVolume: number;
  videoMuted: boolean;
  audioMuted: boolean;
  bookBinding: string;
  [key: string]: unknown;
}

export interface ImageList {
  images: string[];
  index: number;
  archive: string | null;
  dir: string | null;
  prefix: string;
  kind: MediaKind | null;
  depth: number;
}

export interface AudioInfo {
  title: string | null;
  artist: string | null;
  album: string | null;
}

export interface SubtitleCue {
  start: number;
  end: number;
  text: string;
}

export interface SubtitleTrack {
  name: string;
  language: string;
  cues: SubtitleCue[];
}

export interface RemuxAudioTrack {
  name: string | null;
  language: string | null;
  codec: string;
  codecString: string | null;
  default: boolean;
}

export interface MkvOpened {
  id: number;
  videoCodec: string;
  audioCodec: string | null;
  separateAudio: boolean;
  duration: number | null;
  audioTracks: RemuxAudioTrack[];
  audio: number | null;
  transcoded: boolean;
  canTranscode: boolean;
}

export interface AssociationItem {
  ext: string;
  kind: MediaKind;
  associated: boolean;
}

export interface AssociationStatus {
  platform: string;
  items: AssociationItem[];
}

export type RawBytes = ArrayBuffer | Uint8Array | number[];

export interface TauriCommandMap {
  set_window_kind: {
    args: { kind: WindowKindKey | null; fit: boolean };
    result: boolean;
  };
  save_window_geometry: {
    args: Record<never, never>;
    result: undefined;
  };
  list_images: {
    args: { path: string; kind?: MediaKind | null; depth?: number | null };
    result: ImageList;
  };
  read_archive_image: {
    args: { archive: string; entry: string };
    result: RawBytes;
  };
  get_startup_file: {
    args: Record<never, never>;
    result: string | null;
  };
  app_version: {
    args: Record<never, never>;
    result: string;
  };
  load_settings: {
    args: Record<never, never>;
    result: Record<string, unknown> | null;
  };
  save_settings: {
    args: { settings: Record<string, unknown> };
    result: undefined;
  };
  open_settings_window: {
    args: Record<never, never>;
    result: undefined;
  };
  open_shortcuts_window: {
    args: Record<never, never>;
    result: undefined;
  };
  reveal_in_file_manager: {
    args: { path: string };
    result: undefined;
  };
  open_log_folder: {
    args: Record<never, never>;
    result: undefined;
  };
  fit_window_to_image: {
    args: {
      width: number;
      height: number;
      extraWidth?: number | null;
      extraHeight?: number | null;
    };
    result: undefined;
  };
  set_aspect_lock: {
    args: {
      ratio: number | null;
      extraWidth?: number | null;
      extraHeight?: number | null;
    };
    result: undefined;
  };
  mouse_button_down: {
    args: Record<never, never>;
    result: boolean;
  };
  start_window_drag: {
    args: Record<never, never>;
    result: undefined;
  };
  delete_image: {
    args: { path: string };
    result: undefined;
  };
  audio_info: {
    args: { path: string };
    result: AudioInfo;
  };
  audio_artwork: {
    args: { path: string };
    result: RawBytes;
  };
  video_subtitles: {
    args: { path: string };
    result: SubtitleTrack[];
  };
  video_needs_stream: {
    args: { path: string };
    result: boolean;
  };
  mkv_open: {
    args: { path: string; audio: number | null; transcode?: boolean | null };
    result: MkvOpened;
  };
  mkv_init: {
    args: { id: number };
    result: RawBytes;
  };
  mkv_seek: {
    args: { id: number; time: number };
    result: undefined;
  };
  mkv_read: {
    args: { id: number };
    result: RawBytes;
  };
  mkv_cancel_read: {
    args: { id: number };
    result: undefined;
  };
  mkv_close: {
    args: { id: number };
    result: undefined;
  };
  watch_folder: {
    args: { path: string | null; depth: number };
    result: undefined;
  };
  file_association_status: {
    args: Record<never, never>;
    result: AssociationStatus;
  };
  apply_file_associations: {
    args: { exts: string[] };
    result: string;
  };
}

export type TauriInvoke = <K extends keyof TauriCommandMap>(
  cmd: K,
  ...args: keyof TauriCommandMap[K]["args"] extends never
    ? [args?: Record<string, never>]
    : [args: TauriCommandMap[K]["args"]]
) => Promise<TauriCommandMap[K]["result"]>;

export interface TauriEvent<T = unknown> {
  event: string;
  payload: T;
}

export type TauriUnlistenFn = () => void;

export interface TauriWebviewWindow {
  label: string;
  close(): Promise<void>;
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  isMaximized(): Promise<boolean>;
  isFullscreen(): Promise<boolean>;
  setFullscreen(fullscreen: boolean): Promise<void>;
  setAlwaysOnTop(alwaysOnTop: boolean): Promise<void>;
  setTitle(title: string): Promise<void>;
  startDragging(): Promise<void>;
  onResized(handler: () => void): Promise<TauriUnlistenFn>;
  onMoved(handler: () => void): Promise<TauriUnlistenFn>;
  onFocusChanged(handler: (event: TauriEvent<boolean>) => void): Promise<TauriUnlistenFn>;
  listen<T = unknown>(
    event: string,
    handler: (event: TauriEvent<T>) => void
  ): Promise<TauriUnlistenFn>;
}

export interface TauriDialogOpenOptions {
  multiple?: boolean;
  directory?: boolean;
  filters?: Array<{ name: string; extensions: string[] }>;
}

export interface TauriGlobal {
  core?: {
    invoke: TauriInvoke;
    convertFileSrc: (filePath: string, protocol?: string) => string;
  };
  event?: {
    listen: <T = unknown>(
      event: string,
      handler: (event: TauriEvent<T>) => void
    ) => Promise<TauriUnlistenFn>;
    emit: (event: string, payload?: unknown) => Promise<void>;
  };
  dialog?: {
    open: (options?: TauriDialogOpenOptions) => Promise<string | string[] | null>;
  };
  window?: {
    getCurrentWindow: () => TauriWebviewWindow;
  };
}

export interface AudioTrackItem {
  label: string;
  language: string;
  enabled: boolean;
  disabled?: boolean;
  codec?: string;
}

declare global {
  interface Window {
    __TAURI__?: TauriGlobal;
  }
  interface HTMLVideoElement {
    audioTracks?: ArrayLike<AudioTrackItem>;
  }
}
