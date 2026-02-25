import { COLORS } from "./brand";
import { StepDef, StepStatus } from "../components/Terminal/StepsPanel";

// ─── Types ───────────────────────────────────────────────────────────────────

export interface OutputLine {
  text: string;
  appearFrame: number;
  color?: string;
}

export interface ConfirmWidgetState {
  appearFrame: number;
  file: {
    name: string;
    isNew: boolean;
    ruleCount: number;
    previewLines: string[];
  };
  selected: "accept" | "change" | "reject";
}

export interface TuiState {
  /** Remotion frame number when this state becomes active */
  frameStart: number;
  steps: StepDef[];
  activeStepIndex: number;
  outputLines: OutputLine[];
  confirmWidget?: ConfirmWidgetState;
  /** Camera scale applied to the terminal window, 1.0 = normal */
  cameraScale?: number;
  /** Camera vertical offset in px */
  cameraY?: number;
}

// ─── Frame constants ──────────────────────────────────────────────────────────
// All in frames at 60fps. Must match TIMING in brand.ts.

const F = {
  // Scene boundaries
  HOOK_END: 180, // 3s — TUI appears
  REVEAL_END: 360, // 6s — all steps ○, logo settled

  // Step start frames (relative to REVEAL_END)
  ENV_START: 360,
  ENV_END: 510, // 2.5s for env step

  ANALYSIS_START: 510,
  ANALYSIS_END: 720, // 3.5s for analysis

  FETCH_START: 720,
  FETCH_END: 900, // 3s for fetch

  TAILOR_START: 900,
  TAILOR_END: 1080, // 3s for tailoring

  WRITE_START: 1080,
  CONFIRM_APPEAR: 1200, // confirm widget appears mid-write
  ACCEPT_FRAME: 1290, // auto-accept fires
  WRITE_END: 1320, // 4s for write

  SUMMARY_START: 1320, // Summary begins immediately after Write Files
  SUMMARY_END: 1500, // 3s for summary step

  // Post-pipeline
  COMPLETE_START: 1500,
  CTA_START: 1620,
  CLIP_END: 1800,
} as const;

// ─── Output line sets per step ────────────────────────────────────────────────
// Lines stagger in 3 frames apart starting from step's start frame.

function staggerLines(
  texts: Array<{ text: string; color?: string }>,
  startFrame: number,
  offsetPerLine = 3
): OutputLine[] {
  return texts.map((t, i) => ({
    text: t.text,
    appearFrame: startFrame + i * offsetPerLine,
    color: t.color,
  }));
}

const ENV_LINES = staggerLines(
  [
    { text: "  Checking environment...", color: COLORS.textDim },
    { text: "  ✔ Claude CLI runner detected", color: COLORS.borderGreen },
    { text: "  ✔ Authenticated with actual.ai", color: COLORS.borderGreen },
    {
      text: "  ⚠ No .gitignore found — using defaults",
      color: COLORS.warning,
    },
  ],
  F.ENV_START
);

const ANALYSIS_LINES = staggerLines(
  [
    { text: "  Detecting languages..." },
    { text: "  ✔ Rust  (primary)", color: COLORS.borderGreen },
    { text: "  ✔ TOML, Shell", color: COLORS.borderGreen },
    { text: "  Detecting frameworks..." },
    { text: "  ✔ Cargo workspace", color: COLORS.borderGreen },
    { text: "  ✔ ratatui TUI", color: COLORS.borderGreen },
    { text: "  ✔ clap CLI", color: COLORS.borderGreen },
    { text: "  Detecting patterns..." },
    { text: "  ✔ async/await (tokio)", color: COLORS.borderGreen },
    { text: "  ✔ error propagation (anyhow)", color: COLORS.borderGreen },
  ],
  F.ANALYSIS_START,
  18 // 18 frames between lines = slow scroll feel
);

const FETCH_LINES = staggerLines(
  [
    { text: "  Querying actual.ai for matching ADRs..." },
    { text: "  Found 3 applicable ADRs:", color: COLORS.borderGreen },
    { text: "  ─ Error Handling Patterns", color: COLORS.textDim },
    { text: "  ─ Async Runtime Configuration", color: COLORS.textDim },
    { text: "  ─ CLI UX Standards", color: COLORS.textDim },
  ],
  F.FETCH_START,
  20
);

const TAILOR_LINES = staggerLines(
  [
    { text: "  Tailoring ADRs to this codebase..." },
    { text: "  Adapting Error Handling Patterns...", color: COLORS.textDim },
    {
      text: "  Adapting Async Runtime Configuration...",
      color: COLORS.textDim,
    },
    { text: "  Adapting CLI UX Standards...", color: COLORS.textDim },
  ],
  F.TAILOR_START,
  25
);

const WRITE_LINES_PRE_CONFIRM = staggerLines(
  [{ text: "  Confirming file changes..." }],
  F.WRITE_START
);

const WRITE_LINES_POST_ACCEPT = staggerLines(
  [
    { text: "  Writing files..." },
    { text: "  1 created · 0 updated · 0 failed" },
    { text: "    ✔ CLAUDE.md    created   v1", color: COLORS.borderGreen },
  ],
  F.ACCEPT_FRAME,
  6
);

const COMPLETE_LINES = staggerLines(
  [
    { text: "" },
    {
      text: "  Sync complete: 1 created · 0 updated · 0 failed · 0 rejected  [4.8s total]",
      color: COLORS.borderGreen,
    },
  ],
  F.WRITE_END
);

const SUMMARY_LINES = staggerLines(
  [
    { text: "" },
    { text: "  Summary" },
    { text: "  ─ 1 file written", color: COLORS.textDim },
    { text: "      ✔ CLAUDE.md  (new)", color: COLORS.borderGreen },
    { text: "  ─ 3 ADRs applied", color: COLORS.textDim },
    { text: "      ✔ Error Handling Patterns", color: COLORS.borderGreen },
    { text: "      ✔ Async Runtime Configuration", color: COLORS.borderGreen },
    { text: "      ✔ CLI UX Standards", color: COLORS.borderGreen },
  ],
  F.SUMMARY_START + 20, // slight delay after Write Files completes
  18
);

// ─── Step builders ────────────────────────────────────────────────────────────

function allWaiting(): StepDef[] {
  return [
    { label: "Environment", status: "waiting" },
    { label: "Analysis", status: "waiting" },
    { label: "Fetch ADRs", status: "waiting" },
    { label: "Tailoring", status: "waiting" },
    { label: "Write Files", status: "waiting" },
    { label: "Summary", status: "waiting" },
  ];
}

function withStatus(
  base: StepDef[],
  updates: Array<{
    index: number;
    status: StepStatus;
    duration?: string;
    spinnerStartFrame?: number;
    completionFrame?: number;
  }>
): StepDef[] {
  const result = base.map((s) => ({ ...s }));
  for (const u of updates) {
    result[u.index] = { ...result[u.index], ...u };
  }
  return result;
}

// ─── State machine ────────────────────────────────────────────────────────────

export const TUI_STATES: TuiState[] = [
  // 0: Pre-TUI (hook scene) — not used by TUI components but defined for completeness
  {
    frameStart: 0,
    steps: allWaiting(),
    activeStepIndex: 0,
    outputLines: [],
  },

  // 1: TUI revealed, all waiting
  {
    frameStart: F.REVEAL_END,
    steps: allWaiting(),
    activeStepIndex: 0,
    outputLines: [],
  },

  // 2: Environment running
  {
    frameStart: F.ENV_START,
    steps: withStatus(allWaiting(), [
      { index: 0, status: "running", spinnerStartFrame: F.ENV_START },
    ]),
    activeStepIndex: 0,
    outputLines: ENV_LINES,
  },

  // 3: Environment done (warning), Analysis running
  {
    frameStart: F.ENV_END,
    steps: withStatus(allWaiting(), [
      { index: 0, status: "success", duration: "2.5s", completionFrame: F.ENV_END },
      { index: 1, status: "running", spinnerStartFrame: F.ANALYSIS_START },
    ]),
    activeStepIndex: 1,
    outputLines: [...ENV_LINES, ...ANALYSIS_LINES],
  },

  // 4: Analysis done, Fetch running
  {
    frameStart: F.ANALYSIS_END,
    steps: withStatus(allWaiting(), [
      { index: 0, status: "success", duration: "2.5s" },
      { index: 1, status: "success", duration: "3.5s", completionFrame: F.ANALYSIS_END },
      { index: 2, status: "running", spinnerStartFrame: F.FETCH_START },
    ]),
    activeStepIndex: 2,
    outputLines: [...ENV_LINES, ...ANALYSIS_LINES, ...FETCH_LINES],
  },

  // 5: Fetch done, Tailoring running
  {
    frameStart: F.FETCH_END,
    steps: withStatus(allWaiting(), [
      { index: 0, status: "success", duration: "2.5s" },
      { index: 1, status: "success", duration: "3.5s" },
      { index: 2, status: "success", duration: "3.0s", completionFrame: F.FETCH_END },
      { index: 3, status: "running", spinnerStartFrame: F.TAILOR_START },
    ]),
    activeStepIndex: 3,
    outputLines: [...ENV_LINES, ...ANALYSIS_LINES, ...FETCH_LINES, ...TAILOR_LINES],
  },

  // 6: Tailoring done, Write Files running (pre-confirm)
  {
    frameStart: F.TAILOR_END,
    steps: withStatus(allWaiting(), [
      { index: 0, status: "success", duration: "2.5s" },
      { index: 1, status: "success", duration: "3.5s" },
      { index: 2, status: "success", duration: "3.0s" },
      { index: 3, status: "success", duration: "3.0s", completionFrame: F.TAILOR_END },
      { index: 4, status: "running", spinnerStartFrame: F.WRITE_START },
    ]),
    activeStepIndex: 4,
    outputLines: [
      ...ENV_LINES,
      ...ANALYSIS_LINES,
      ...FETCH_LINES,
      ...TAILOR_LINES,
      ...WRITE_LINES_PRE_CONFIRM,
    ],
  },

  // 7: Confirm widget appears (camera punches in)
  {
    frameStart: F.CONFIRM_APPEAR,
    steps: withStatus(allWaiting(), [
      { index: 0, status: "success", duration: "2.5s" },
      { index: 1, status: "success", duration: "3.5s" },
      { index: 2, status: "success", duration: "3.0s" },
      { index: 3, status: "success", duration: "3.0s" },
      { index: 4, status: "running", spinnerStartFrame: F.WRITE_START },
    ]),
    activeStepIndex: 4,
    outputLines: [
      ...ENV_LINES,
      ...ANALYSIS_LINES,
      ...FETCH_LINES,
      ...TAILOR_LINES,
      ...WRITE_LINES_PRE_CONFIRM,
    ],
    confirmWidget: {
      appearFrame: F.CONFIRM_APPEAR,
      file: {
        name: "CLAUDE.md",
        isNew: true,
        ruleCount: 3,
        previewLines: [
          "├─  + ## Error Handling Patterns",
          "├─  + ## Async Runtime Configuration",
          "└─  + ## CLI UX Standards",
        ],
      },
      selected: "accept",
    },
    cameraScale: 1.15,
    cameraY: -40,
  },

  // 8: Write done, Summary running
  {
    frameStart: F.SUMMARY_START,
    steps: withStatus(allWaiting(), [
      { index: 0, status: "success", duration: "2.5s" },
      { index: 1, status: "success", duration: "3.5s" },
      { index: 2, status: "success", duration: "3.0s" },
      { index: 3, status: "success", duration: "3.0s" },
      { index: 4, status: "success", duration: "4.0s", completionFrame: F.WRITE_END },
      { index: 5, status: "running", spinnerStartFrame: F.SUMMARY_START },
    ]),
    activeStepIndex: 5,
    outputLines: [
      ...ENV_LINES,
      ...ANALYSIS_LINES,
      ...FETCH_LINES,
      ...TAILOR_LINES,
      ...WRITE_LINES_PRE_CONFIRM,
      ...WRITE_LINES_POST_ACCEPT,
      ...COMPLETE_LINES,
      ...SUMMARY_LINES,
    ],
    cameraScale: 1.0,
    cameraY: 0,
  },

  // 9: Summary done — all steps complete
  {
    frameStart: F.SUMMARY_END,
    steps: withStatus(allWaiting(), [
      { index: 0, status: "success", duration: "2.5s" },
      { index: 1, status: "success", duration: "3.5s" },
      { index: 2, status: "success", duration: "3.0s" },
      { index: 3, status: "success", duration: "3.0s" },
      { index: 4, status: "success", duration: "4.0s" },
      { index: 5, status: "success", duration: "3.0s", completionFrame: F.SUMMARY_END },
    ]),
    activeStepIndex: 5,
    outputLines: [
      ...ENV_LINES,
      ...ANALYSIS_LINES,
      ...FETCH_LINES,
      ...TAILOR_LINES,
      ...WRITE_LINES_PRE_CONFIRM,
      ...WRITE_LINES_POST_ACCEPT,
      ...COMPLETE_LINES,
      ...SUMMARY_LINES,
    ],
    cameraScale: 1.0,
    cameraY: 0,
  },
];

// ─── Helper: get active state for a given frame ───────────────────────────────

export function getStateAtFrame(frame: number): TuiState {
  // Find the last state whose frameStart <= frame
  let active = TUI_STATES[0];
  for (const state of TUI_STATES) {
    if (state.frameStart <= frame) {
      active = state;
    } else {
      break;
    }
  }
  return active;
}

// ─── Helper: interpolate camera from state machine ───────────────────────────

export function getCameraAtFrame(frame: number): { scale: number; y: number } {
  const state = getStateAtFrame(frame);
  return {
    scale: state.cameraScale ?? 1.0,
    y: state.cameraY ?? 0,
  };
}

// Re-export frame constants for use in scenes
export { F as FRAMES };
