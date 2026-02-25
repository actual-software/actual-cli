export const COLORS = {
  background: "#0a0c0b",
  surface: "#111512",
  borderGreen: "#00FB7E",
  borderTeal: "#179CA9",
  textPrimary: "#e8f0ea",
  textDim: "#6b7c6e",
  warning: "#FFD166",
  error: "#FF4B4B",
  success: "#00FB7E",
} as const;

export const FONTS = {
  mono: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', monospace",
} as const;

export const TIMING = {
  fps: 60,
  // Scene durations in frames
  hookDuration: 180,       // 3s
  revealDuration: 180,     // 3s
  pipelineDuration: 960,   // 16s
  completeDuration: 300,   // 5s
  ctaDuration: 180,        // 3s
  // Step durations in frames
  envStepFrames: 150,
  analysisStepFrames: 210,
  fetchStepFrames: 180,
  tailoringStepFrames: 180,
  writeStepFrames: 240,
} as const;

export const SPRING_CONFIGS = {
  slideIn: { mass: 0.5, damping: 12 },
  settle: { mass: 0.8, damping: 16 },
  punchIn: { mass: 0.6, damping: 14 },
  glowBurst: { mass: 0.4, damping: 8 },
} as const;
