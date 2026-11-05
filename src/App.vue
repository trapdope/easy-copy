<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  AlertCircle,
  ClipboardList,
  Command,
  Image as ImageIcon,
  Keyboard,
  Lock,
  LockOpen,
  MessageSquareText,
  MousePointerClick,
  Power,
  Search,
  Settings,
  Trash2,
  X,
} from "@lucide/vue";

type ClipboardKind = "text" | "image";

type ClipboardItem = {
  id: number;
  kind: ClipboardKind;
  preview: string;
  text: string | null;
  charCount: number | null;
  imageDataUrl: string | null;
  width: number | null;
  height: number | null;
  byteCount: number | null;
  sourceApp: string | null;
  createdAt: number;
  locked: boolean;
  note: string | null;
};

type PasteResult = {
  pasted: boolean;
};

type AppSettings = {
  launchAtLogin: boolean;
  shortcut: string;
};

const DEFAULT_SETTINGS: AppSettings = {
  launchAtLogin: false,
  shortcut: "Cmd+Shift+V",
};
const READY_STATUS = "就绪";

const history = ref<ClipboardItem[]>([]);
const query = ref("");
const selectedIndex = ref(0);
const isPasting = ref(false);
const isSettingsOpen = ref(false);
const isSavingSettings = ref(false);
const isRecordingShortcut = ref(false);
const editingNoteItemId = ref<number | null>(null);
const noteDraft = ref("");
const isSavingNote = ref(false);
const noteErrorMessage = ref("");
const errorMessage = ref("");
const settingsErrorMessage = ref("");
const statusMessage = ref(READY_STATUS);
const appSettings = ref<AppSettings>({ ...DEFAULT_SETTINGS });

let unlistenHistory: UnlistenFn | undefined;
let unlistenOpened: UnlistenFn | undefined;
let unlistenSettingsOpened: UnlistenFn | undefined;
let unlistenSettingsUpdated: UnlistenFn | undefined;
let statusTimer: number | undefined;

const filteredItems = computed(() => {
  const needle = query.value.trim().toLowerCase();

  if (!needle) {
    return history.value;
  }

  return history.value.filter((item) => fuzzyMatch(itemSearchText(item), needle));
});

const selectedItem = computed(() => filteredItems.value[selectedIndex.value]);
const shortcutInputValue = computed(() =>
  isRecordingShortcut.value ? "按下新的快捷键" : appSettings.value.shortcut,
);

watch(query, () => {
  selectedIndex.value = 0;
});

watch(filteredItems, (items) => {
  if (items.length === 0) {
    selectedIndex.value = 0;
    return;
  }

  selectedIndex.value = Math.min(selectedIndex.value, items.length - 1);
});

onMounted(async () => {
  history.value = await invoke<ClipboardItem[]>("get_clipboard_history");
  appSettings.value = await invoke<AppSettings>("get_app_settings");

  unlistenHistory = await listen<ClipboardItem[]>(
    "clipboard-history-updated",
    (event) => {
      history.value = event.payload;
    },
  );

  unlistenOpened = await listen("palette-opened", () => {
    query.value = "";
    selectedIndex.value = 0;
    errorMessage.value = "";
    settingsErrorMessage.value = "";
    isSettingsOpen.value = false;
    isRecordingShortcut.value = false;
    setStatus(READY_STATUS);
  });

  unlistenSettingsOpened = await listen("settings-opened", () => {
    settingsErrorMessage.value = "";
    isSettingsOpen.value = true;
    isRecordingShortcut.value = false;
  });

  unlistenSettingsUpdated = await listen<AppSettings>(
    "app-settings-updated",
    (event) => {
      appSettings.value = event.payload;
    },
  );

  window.addEventListener("keydown", handleKeydown);
});

onBeforeUnmount(() => {
  unlistenHistory?.();
  unlistenOpened?.();
  unlistenSettingsOpened?.();
  unlistenSettingsUpdated?.();
  window.removeEventListener("keydown", handleKeydown);

  if (statusTimer) {
    window.clearTimeout(statusTimer);
  }
});

function handleKeydown(event: KeyboardEvent) {
  if (editingNoteItemId.value !== null) {
    if (event.key === "Escape") {
      event.preventDefault();
      closeNoteEditor();
    }
    return;
  }

  if (isRecordingShortcut.value) {
    void handleShortcutRecording(event);
    return;
  }

  if (isSettingsOpen.value) {
    if (event.key === "Escape") {
      event.preventDefault();
      isSettingsOpen.value = false;
    }

    return;
  }

  const isEditableTarget = isKeyboardEventFromEditable(event);

  if (event.key === "ArrowDown") {
    event.preventDefault();
    selectNext();
    return;
  }

  if (event.key === "ArrowUp") {
    event.preventDefault();
    selectPrevious();
    return;
  }

  if (event.key === "Enter") {
    if (isEditableTarget) {
      return;
    }

    event.preventDefault();
    void pasteSelected();
    return;
  }

  if (event.key === "Escape") {
    event.preventDefault();
    void invoke("hide_palette");
  }
}

async function saveSettings(nextSettings: AppSettings) {
  if (isSavingSettings.value) {
    return;
  }

  isSavingSettings.value = true;
  settingsErrorMessage.value = "";

  try {
    appSettings.value = await invoke<AppSettings>("update_app_settings", {
      settings: nextSettings,
    });
    setStatus("已保存");
  } catch (error) {
    settingsErrorMessage.value =
      error instanceof Error ? error.message : String(error || "设置保存失败。");
  } finally {
    isSavingSettings.value = false;
    isRecordingShortcut.value = false;
  }
}

async function toggleLaunchAtLogin() {
  await saveSettings({
    ...appSettings.value,
    launchAtLogin: !appSettings.value.launchAtLogin,
  });
}

function toggleSettingsPanel() {
  isSettingsOpen.value = !isSettingsOpen.value;
  settingsErrorMessage.value = "";
  isRecordingShortcut.value = false;
}

function startShortcutRecording() {
  settingsErrorMessage.value = "";
  isRecordingShortcut.value = true;
}

function stopShortcutRecording() {
  isRecordingShortcut.value = false;
}

async function resetShortcut() {
  await saveSettings({
    ...appSettings.value,
    shortcut: DEFAULT_SETTINGS.shortcut,
  });
}

function handleShortcutFieldKeydown(event: KeyboardEvent) {
  event.stopPropagation();

  if (!isRecordingShortcut.value) {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      startShortcutRecording();
    }

    return;
  }

  void handleShortcutRecording(event);
}

async function handleShortcutRecording(event: KeyboardEvent) {
  event.preventDefault();
  event.stopPropagation();

  if (event.key === "Escape") {
    isRecordingShortcut.value = false;
    return;
  }

  const shortcut = shortcutFromKeyboardEvent(event);

  if (!shortcut) {
    settingsErrorMessage.value = "快捷键至少需要 Cmd、Ctrl 或 Option 加一个按键。";
    return;
  }

  await saveSettings({
    ...appSettings.value,
    shortcut,
  });
}

function shortcutFromKeyboardEvent(event: KeyboardEvent) {
  const key = shortcutKeyFromEvent(event);

  if (!key) {
    return "";
  }

  if (!event.metaKey && !event.ctrlKey && !event.altKey) {
    return "";
  }

  const parts: string[] = [];

  if (event.metaKey) {
    parts.push("Cmd");
  }
  if (event.ctrlKey) {
    parts.push("Ctrl");
  }
  if (event.altKey) {
    parts.push("Option");
  }
  if (event.shiftKey) {
    parts.push("Shift");
  }

  parts.push(key);
  return parts.join("+");
}

function shortcutKeyFromEvent(event: KeyboardEvent) {
  const modifierKeys = new Set([
    "Alt",
    "Control",
    "Meta",
    "Shift",
    "CapsLock",
    "Fn",
  ]);

  if (modifierKeys.has(event.key)) {
    return "";
  }

  if (/^Key[A-Z]$/.test(event.code)) {
    return event.code.slice(3);
  }

  if (/^Digit[0-9]$/.test(event.code)) {
    return event.code.slice(5);
  }

  if (/^F\d{1,2}$/.test(event.code)) {
    return event.code;
  }

  const codeMap: Record<string, string> = {
    ArrowDown: "Down",
    ArrowLeft: "Left",
    ArrowRight: "Right",
    ArrowUp: "Up",
    Backquote: "`",
    Backslash: "\\",
    BracketLeft: "[",
    BracketRight: "]",
    Comma: ",",
    Delete: "Delete",
    End: "End",
    Enter: "Enter",
    Equal: "=",
    Escape: "Escape",
    Home: "Home",
    Insert: "Insert",
    Minus: "-",
    PageDown: "PageDown",
    PageUp: "PageUp",
    Period: ".",
    Quote: "'",
    Semicolon: ";",
    Slash: "/",
    Space: "Space",
    Tab: "Tab",
  };

  return codeMap[event.code] || event.code;
}

function isKeyboardEventFromEditable(event: KeyboardEvent) {
  const target = event.target;

  if (!(target instanceof HTMLElement)) {
    return false;
  }

  return (
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    target.isContentEditable
  );
}

function selectNext() {
  if (filteredItems.value.length === 0) {
    return;
  }

  selectedIndex.value = (selectedIndex.value + 1) % filteredItems.value.length;
  scrollSelectedIntoView();
}

function selectPrevious() {
  if (filteredItems.value.length === 0) {
    return;
  }

  selectedIndex.value =
    (selectedIndex.value - 1 + filteredItems.value.length) %
    filteredItems.value.length;
  scrollSelectedIntoView();
}

async function pasteSelected() {
  if (!selectedItem.value) {
    return;
  }

  await pasteItem(selectedItem.value);
}

async function pasteItem(item: ClipboardItem) {
  if (isPasting.value) {
    return;
  }

  isPasting.value = true;
  errorMessage.value = "";

  try {
    const result = await invoke<PasteResult>("paste_clipboard_item", { id: item.id });
    setStatus(result.pasted ? "已粘贴" : "已复制");
  } catch (error) {
    errorMessage.value =
      error instanceof Error ? error.message : String(error || "粘贴失败。");
  } finally {
    isPasting.value = false;
  }
}

async function toggleLocked(item: ClipboardItem) {
  errorMessage.value = "";

  try {
    await invoke("set_clipboard_item_locked", { id: item.id, locked: !item.locked });
    setStatus(item.locked ? "已取消锁定" : "已锁定，将长期保留");
  } catch (error) {
    errorMessage.value =
      error instanceof Error ? error.message : String(error || "锁定状态保存失败。");
  }
}

function editNote(item: ClipboardItem) {
  editingNoteItemId.value = item.id;
  noteDraft.value = item.note || "";
  noteErrorMessage.value = "";
}

function closeNoteEditor() {
  if (isSavingNote.value) {
    return;
  }
  editingNoteItemId.value = null;
  noteDraft.value = "";
  noteErrorMessage.value = "";
}

async function saveNote() {
  const id = editingNoteItemId.value;
  if (id === null || isSavingNote.value) {
    return;
  }

  isSavingNote.value = true;
  noteErrorMessage.value = "";

  try {
    const note = noteDraft.value.trim();
    await invoke("set_clipboard_item_note", { id, note });
    editingNoteItemId.value = null;
    noteDraft.value = "";
    setStatus(note ? "备注已保存" : "备注已清除");
  } catch (error) {
    noteErrorMessage.value =
      error instanceof Error ? error.message : String(error || "备注保存失败。");
  } finally {
    isSavingNote.value = false;
  }
}

async function deleteItem(id: number) {
  errorMessage.value = "";

  try {
    await invoke("delete_clipboard_item", { id });
    setStatus("已删除");
  } catch (error) {
    errorMessage.value =
      error instanceof Error ? error.message : String(error || "删除失败。");
  }
}

async function openAccessibilitySettings() {
  await invoke("open_accessibility_settings");
}

function startWindowDrag(event: MouseEvent) {
  if (event.button !== 0) {
    return;
  }

  const target = event.target;

  if (target instanceof Element && target.closest("[data-no-drag]")) {
    return;
  }

  void getCurrentWindow().startDragging().catch(() => undefined);
}

function fuzzyMatch(text: string, needle: string) {
  const haystack = text.toLowerCase();
  const tokens = needle.split(/\s+/).filter(Boolean);

  return tokens.every(
    (token) => haystack.includes(token) || isSubsequence(token, haystack),
  );
}

function isSubsequence(needle: string, haystack: string) {
  let index = 0;

  for (const char of haystack) {
    if (char === needle[index]) {
      index += 1;
    }

    if (index === needle.length) {
      return true;
    }
  }

  return false;
}

function scrollSelectedIntoView() {
  requestAnimationFrame(() => {
    document
      .querySelector<HTMLElement>("[data-selected='true']")
      ?.scrollIntoView({ block: "nearest" });
  });
}

function itemSearchText(item: ClipboardItem) {
  if (item.kind === "text") {
    return `${item.text || item.preview} ${item.sourceApp || ""} ${item.note || ""}`;
  }

  return `${item.preview} ${item.width || ""} ${item.height || ""} ${
    item.sourceApp || ""
  } ${item.note || ""}`;
}

function itemPrimaryLabel(item: ClipboardItem) {
  return item.kind === "image" ? "图片" : item.preview;
}

function itemMeta(item: ClipboardItem) {
  if (item.kind === "image") {
    const size =
      item.width && item.height ? `${item.width}x${item.height}` : "未知尺寸";
    return `${size} · ${formatBytes(item.byteCount || 0)} · ${formatTime(
      item.createdAt,
    )}`;
  }

  return `${item.charCount || 0} 字符 · ${formatTime(item.createdAt)}`;
}

function itemSourceLabel(item: ClipboardItem) {
  return item.sourceApp ? `来源：${item.sourceApp}` : "来源未知";
}

function formatTime(timestamp: number) {
  return new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(timestamp));
}

function formatBytes(bytes: number) {
  if (bytes < 1024) {
    return `${bytes} B`;
  }

  const kb = bytes / 1024;

  if (kb < 1024) {
    return `${kb.toFixed(1)} KB`;
  }

  return `${(kb / 1024).toFixed(1)} MB`;
}

function setStatus(value: string) {
  statusMessage.value = value;

  if (statusTimer) {
    window.clearTimeout(statusTimer);
  }

  if (value !== READY_STATUS) {
    statusTimer = window.setTimeout(() => {
      statusMessage.value = READY_STATUS;
    }, 1500);
  }
}
</script>

<template>
  <main class="h-[100dvh] w-screen bg-transparent p-1.5 text-zinc-900">
    <section
      class="relative flex h-full flex-col overflow-hidden rounded-xl border border-zinc-300/90 bg-white/[0.9] backdrop-blur-2xl"
    >
      <header class="border-b border-white/70 bg-white/45 px-3.5 pb-3 pt-3">
        <div
          data-tauri-drag-region
          class="flex cursor-grab items-center justify-between gap-3 active:cursor-grabbing"
          :class="isSettingsOpen ? 'mb-0' : 'mb-3'"
          @mousedown="startWindowDrag"
        >
          <div data-tauri-drag-region class="flex min-w-0 items-center gap-2.5">
            <div
              data-tauri-drag-region
              class="flex size-7 shrink-0 items-center justify-center rounded-md border border-white/70 bg-white/70 text-zinc-700"
            >
              <ClipboardList :size="15" :stroke-width="1.8" />
            </div>
            <div data-tauri-drag-region class="min-w-0">
              <h1
                data-tauri-drag-region
                class="truncate text-[13px] font-semibold leading-5 tracking-normal"
              >
                EasyCopy
              </h1>
              <p data-tauri-drag-region class="text-[11px] leading-4 text-zinc-500">
                {{ filteredItems.length }} / {{ history.length }}
              </p>
            </div>
          </div>

          <div data-no-drag class="flex shrink-0 items-center gap-1">
            <button
              class="flex size-7 items-center justify-center rounded-md text-zinc-500 transition hover:bg-zinc-100/80 hover:text-zinc-800 active:scale-[0.98]"
              :class="isSettingsOpen ? 'bg-zinc-100 text-zinc-800' : ''"
              type="button"
              title="选项"
              aria-label="打开选项"
              @click="toggleSettingsPanel"
            >
              <Settings :size="15" :stroke-width="1.8" />
            </button>

            <button
              class="flex size-7 items-center justify-center rounded-md text-zinc-500 transition hover:bg-zinc-100/80 hover:text-zinc-800 active:scale-[0.98]"
              type="button"
              title="隐藏"
              aria-label="隐藏 EasyCopy"
              @click="invoke('hide_palette')"
            >
              <X :size="15" :stroke-width="1.8" />
            </button>
          </div>
        </div>

        <label
          v-if="!isSettingsOpen"
          data-no-drag
          class="flex h-9 items-center gap-2 rounded-lg border border-white/75 bg-white/75 px-2.5 transition focus-within:border-zinc-300"
        >
          <Search class="shrink-0 text-zinc-400" :size="15" :stroke-width="1.8" />
          <input
            v-model="query"
            class="h-full min-w-0 flex-1 bg-transparent text-[13px] outline-none placeholder:text-zinc-400"
            placeholder="搜索剪贴板"
            spellcheck="false"
          />
        </label>
      </header>

      <div
        v-if="errorMessage"
        class="border-b border-amber-200/80 bg-amber-50/95 px-3.5 py-2"
      >
        <div class="flex items-start gap-2 text-[12px] leading-5 text-amber-900">
          <AlertCircle class="mt-0.5 shrink-0" :size="14" :stroke-width="1.8" />
          <p class="min-w-0 flex-1">{{ errorMessage }}</p>
          <button
            class="flex size-6 shrink-0 items-center justify-center rounded-md text-amber-900 transition hover:bg-amber-100 active:scale-[0.98]"
            type="button"
            title="辅助功能权限"
            aria-label="打开辅助功能权限"
            @click="openAccessibilitySettings"
          >
            <Settings :size="14" :stroke-width="1.8" />
          </button>
        </div>
      </div>

      <div v-if="isSettingsOpen" class="min-h-0 flex-1 overflow-y-auto p-3.5">
        <div
          v-if="settingsErrorMessage"
          class="mb-3 rounded-lg border border-amber-200/80 bg-amber-50/95 px-3 py-2 text-[12px] leading-5 text-amber-900"
        >
          {{ settingsErrorMessage }}
        </div>

        <div class="space-y-3">
          <section
            class="overflow-hidden rounded-xl border border-white/80 bg-white/65 backdrop-blur-xl"
          >
            <button
              class="flex min-h-[58px] w-full items-center gap-3 px-3 text-left transition hover:bg-zinc-100/70 active:scale-[0.995] disabled:opacity-60"
              type="button"
              role="switch"
              :aria-checked="appSettings.launchAtLogin"
              :disabled="isSavingSettings"
              @click="toggleLaunchAtLogin"
            >
              <span
                class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-zinc-200/80 bg-white/80 text-zinc-600"
              >
                <Power :size="15" :stroke-width="1.8" />
              </span>
              <span class="min-w-0 flex-1">
                <span class="block text-[13px] font-medium leading-5 text-zinc-800">
                  开机启动
                </span>
                <span class="block text-[11px] leading-4 text-zinc-500">
                  {{ appSettings.launchAtLogin ? "已开启" : "已关闭" }}
                </span>
              </span>
              <span
                class="relative h-5 w-9 shrink-0 rounded-full transition"
                :class="appSettings.launchAtLogin ? 'bg-zinc-700' : 'bg-zinc-300/80'"
              >
                <span
                  class="absolute left-0.5 top-0.5 size-4 rounded-full bg-white transition"
                  :class="appSettings.launchAtLogin ? 'translate-x-4' : 'translate-x-0'"
                />
              </span>
            </button>
          </section>

          <section
            class="rounded-xl border border-white/80 bg-white/65 p-3 backdrop-blur-xl"
          >
            <div class="mb-2.5 flex items-center gap-2.5">
              <span
                class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-zinc-200/80 bg-white/80 text-zinc-600"
              >
                <Keyboard :size="15" :stroke-width="1.8" />
              </span>
              <div class="min-w-0 flex-1">
                <div class="text-[13px] font-medium leading-5 text-zinc-800">
                  唤醒快捷键
                </div>
                <div
                  class="truncate text-[11px] leading-4"
                  :class="isRecordingShortcut ? 'text-[#155BD0]' : 'text-zinc-500'"
                >
                  {{ isRecordingShortcut ? "录制中" : "用于呼出 EasyCopy" }}
                </div>
              </div>
            </div>

            <div class="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-1.5">
              <label
                class="flex h-[30px] min-w-0 items-center gap-1.5 rounded-md border px-2 transition"
                :class="
                  isRecordingShortcut
                    ? 'border-[#155BD0]/60 bg-white ring-2 ring-[#155BD0]/20'
                    : 'border-zinc-200/90 bg-white/80 hover:bg-white'
                "
              >
                <Command
                  class="shrink-0"
                  :class="isRecordingShortcut ? 'text-[#155BD0]' : 'text-zinc-400'"
                  :size="13"
                  :stroke-width="1.8"
                />
                <input
                  class="h-full min-w-0 flex-1 cursor-text bg-transparent text-[11px] font-normal text-zinc-800 outline-none placeholder:text-zinc-400"
                  :class="isRecordingShortcut ? 'text-[#155BD0]' : ''"
                  :value="shortcutInputValue"
                  readonly
                  spellcheck="false"
                  aria-label="唤醒快捷键"
                  @click="startShortcutRecording"
                  @focus="startShortcutRecording"
                  @blur="stopShortcutRecording"
                  @keydown.stop="handleShortcutFieldKeydown"
                />
                <span
                  v-if="isRecordingShortcut"
                  class="size-1 shrink-0 rounded-full bg-[#155BD0]"
                />
              </label>

              <button
                class="h-[30px] rounded-md border border-zinc-200/90 bg-white/80 px-2.5 text-[10px] font-normal text-zinc-700 transition hover:bg-zinc-100 active:scale-[0.98] disabled:opacity-60"
                type="button"
                :disabled="isSavingSettings"
                @click="resetShortcut"
              >
                重置
              </button>
            </div>
          </section>
        </div>
      </div>

      <div v-else class="min-h-0 flex-1 overflow-y-auto p-2">
        <div
          v-if="filteredItems.length === 0"
          class="flex h-full flex-col items-center justify-center px-8 text-center"
        >
          <div
            class="mb-3 flex size-10 items-center justify-center rounded-lg border border-white/70 bg-white/75 text-zinc-500"
          >
            <ClipboardList :size="18" :stroke-width="1.8" />
          </div>
          <p class="text-[13px] font-medium text-zinc-700">
            {{ history.length === 0 ? "还没有剪贴板记录" : "没有匹配项" }}
          </p>
        </div>

        <ul v-else class="space-y-1">
          <li v-for="(item, index) in filteredItems" :key="item.id">
            <div
              class="grid grid-cols-[1fr_auto] overflow-hidden rounded-lg transition"
              :class="
                index === selectedIndex
                  ? 'bg-zinc-100/95 text-zinc-900'
                  : 'text-zinc-800 hover:bg-zinc-100/75'
              "
              :data-selected="index === selectedIndex"
            >
              <button
                class="grid min-w-0 grid-cols-[auto_1fr] items-center gap-2.5 px-2.5 py-2 text-left active:scale-[0.995]"
                type="button"
                @pointerdown.prevent.stop="pasteItem(item)"
                @click.prevent.stop="pasteItem(item)"
              >
                <span
                  v-if="item.kind === 'image'"
                  class="flex size-12 shrink-0 items-center justify-center overflow-hidden rounded-md border"
                  :class="
                    index === selectedIndex
                      ? 'border-zinc-200 bg-white/80'
                      : 'border-zinc-200 bg-white'
                  "
                >
                  <img
                    v-if="item.imageDataUrl"
                    class="h-full w-full object-cover"
                    :src="item.imageDataUrl"
                    alt=""
                  />
                  <ImageIcon v-else :size="17" :stroke-width="1.8" />
                </span>
                <span
                  v-else
                  class="flex size-8 shrink-0 items-center justify-center rounded-md border"
                  :class="
                    index === selectedIndex
                      ? 'border-zinc-200 bg-white/80 text-zinc-600'
                      : 'border-zinc-200 bg-white text-zinc-500'
                  "
                >
                  <ClipboardList :size="15" :stroke-width="1.8" />
                </span>

                <span class="min-w-0">
                  <span class="line-clamp-2 text-[13px] font-medium leading-[18px]">
                    {{ itemPrimaryLabel(item) }}
                  </span>
                  <span
                    class="mt-1 block truncate text-[11px] leading-4"
                    :class="index === selectedIndex ? 'text-zinc-600' : 'text-zinc-500'"
                  >
                    {{ itemMeta(item) }}
                  </span>
                  <span
                    class="mt-0.5 block truncate text-[11px] leading-4"
                    :class="index === selectedIndex ? 'text-zinc-500' : 'text-zinc-500'"
                  >
                    {{ itemSourceLabel(item) }}
                  </span>
                  <span
                    v-if="item.note"
                    class="mt-0.5 block truncate text-[11px] leading-4 text-[#155BD0]"
                    :title="item.note"
                  >
                    备注：{{ item.note }}
                  </span>
                </span>
              </button>

              <div class="flex shrink-0 items-stretch">
                <button
                  class="flex w-7 items-center justify-center transition active:scale-[0.96]"
                  :class="item.locked ? 'text-[#155BD0]' : 'text-zinc-400 hover:bg-zinc-100 hover:text-zinc-700'"
                  type="button"
                  :title="item.locked ? '取消锁定' : '锁定并长期保留'"
                  :aria-label="item.locked ? '取消锁定记录' : '锁定记录'"
                  @pointerdown.stop
                  @click.stop="toggleLocked(item)"
                >
                  <component :is="item.locked ? Lock : LockOpen" :size="14" :stroke-width="1.8" />
                </button>
                <button
                  class="flex w-7 items-center justify-center transition active:scale-[0.96]"
                  :class="item.note ? 'text-[#155BD0]' : 'text-zinc-400 hover:bg-zinc-100 hover:text-zinc-700'"
                  type="button"
                  :title="item.note ? '编辑备注' : '添加备注'"
                  aria-label="添加或编辑备注"
                  @pointerdown.stop
                  @click.stop="editNote(item)"
                >
                  <MessageSquareText :size="14" :stroke-width="1.8" />
                </button>
                <button
                  class="flex w-7 items-center justify-center transition active:scale-[0.96]"
                  :class="index === selectedIndex ? 'text-zinc-500 hover:bg-zinc-200/70 hover:text-zinc-800' : 'text-zinc-400 hover:bg-zinc-100 hover:text-zinc-700'"
                  type="button"
                  title="删除"
                  aria-label="删除记录"
                  @pointerdown.stop
                  @click.stop="deleteItem(item.id)"
                >
                  <Trash2 :size="14" :stroke-width="1.8" />
                </button>
              </div>
            </div>
          </li>
        </ul>
      </div>

      <footer
        class="flex h-9 shrink-0 items-center justify-between border-t border-white/70 bg-white/35 px-3.5 text-[11px] text-zinc-500"
      >
        <span class="flex items-center gap-1.5">
          <MousePointerClick :size="13" :stroke-width="1.8" />
          <span>{{ isPasting ? "处理中" : isSavingSettings ? "保存中" : statusMessage }}</span>
        </span>
        <span v-if="isSettingsOpen">{{ appSettings.shortcut }}</span>
        <span v-else>{{ selectedItem ? selectedIndex + 1 : 0 }} / {{ filteredItems.length }}</span>
      </footer>

      <div
        v-if="editingNoteItemId !== null"
        class="absolute inset-0 z-30 flex items-center justify-center bg-zinc-900/20 p-5 backdrop-blur-[2px]"
        @click.self="closeNoteEditor"
      >
        <form
          class="w-full max-w-[320px] rounded-xl border border-zinc-200 bg-white p-4 shadow-xl"
          role="dialog"
          aria-modal="true"
          aria-labelledby="note-editor-title"
          @submit.prevent="saveNote"
        >
          <div class="mb-3 flex items-center justify-between">
            <h2 id="note-editor-title" class="text-[13px] font-semibold text-zinc-800">
              备注这条内容的作用
            </h2>
            <button
              class="flex size-7 items-center justify-center rounded-md text-zinc-500 hover:bg-zinc-100"
              type="button"
              aria-label="关闭备注编辑"
              :disabled="isSavingNote"
              @click="closeNoteEditor"
            >
              <X :size="15" :stroke-width="1.8" />
            </button>
          </div>
          <textarea
            v-model="noteDraft"
            class="min-h-24 w-full resize-none rounded-lg border border-zinc-200 bg-zinc-50 px-3 py-2 text-[13px] leading-5 text-zinc-800 outline-none transition placeholder:text-zinc-400 focus:border-zinc-400 focus:bg-white"
            placeholder="例如：公司邮箱签名、常用地址……"
            maxlength="500"
            autofocus
          />
          <p v-if="noteErrorMessage" class="mt-2 text-[11px] leading-4 text-red-600">
            {{ noteErrorMessage }}
          </p>
          <div class="mt-3 flex justify-end gap-2">
            <button
              class="h-8 rounded-md px-3 text-[12px] text-zinc-600 transition hover:bg-zinc-100 disabled:opacity-50"
              type="button"
              :disabled="isSavingNote"
              @click="closeNoteEditor"
            >
              取消
            </button>
            <button
              class="h-8 rounded-md bg-zinc-800 px-3 text-[12px] font-medium text-white transition hover:bg-zinc-700 disabled:opacity-50"
              type="submit"
              :disabled="isSavingNote"
            >
              {{ isSavingNote ? "保存中…" : "保存" }}
            </button>
          </div>
        </form>
      </div>
    </section>
  </main>
</template>
