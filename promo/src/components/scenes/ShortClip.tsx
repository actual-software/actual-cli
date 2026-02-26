import React from "react";
import { Sequence, useCurrentFrame, useVideoConfig, interpolate } from "remotion";
import { TerminalWindow } from "../Terminal/TerminalWindow";
import { TuiLayout } from "../Terminal/TuiLayout";
import { SceneComplete } from "./SceneComplete";
import { SceneCta } from "./SceneCta";
import { FilmGrain } from "../effects/FilmGrain";
import { Vignette } from "../effects/Vignette";
import { COLORS, FONTS } from "../../data/brand";
import { COPY } from "../../data/copy";
import { getStateAtFrame, FRAMES } from "../../data/tui-states";

// Instant hook: terminal already showing command, fades in over 15 frames
const InstantHook: React.FC = () => {
  const frame = useCurrentFrame();
  const opacity = interpolate(frame, [0, 15], [0, 1], {
    extrapolateRight: "clamp",
  });
  return (
    <div
      style={{
        width: "100%",
        height: "100%",
        background: COLORS.background,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        opacity,
      }}
    >
      <TerminalWindow width={1200} height={640}>
        <div
          style={{
            fontFamily: FONTS.mono,
            fontSize: 14,
            color: COLORS.textPrimary,
            padding: "24px 20px",
          }}
        >
          <span style={{ color: COLORS.borderGreen }}>{COPY.shell.prompt}</span>
          <span style={{ color: COLORS.textDim }}> ❯ </span>
          <span>{COPY.shell.command}</span>
        </div>
      </TerminalWindow>
    </div>
  );
};

// Pipeline compressed to 660 frames, mapping the full 960-frame pipeline range
const FastPipeline: React.FC = () => {
  const frame = useCurrentFrame();
  const pipelineDuration = FRAMES.WRITE_END - FRAMES.REVEAL_END; // 960
  const absoluteFrame =
    FRAMES.REVEAL_END + Math.floor((frame * pipelineDuration) / 660);
  const state = getStateAtFrame(absoluteFrame);

  const confirmAbsRelFrame = FRAMES.CONFIRM_APPEAR - FRAMES.REVEAL_END;
  const confirmShortFrame = Math.floor(
    (confirmAbsRelFrame * 660) / pipelineDuration
  );

  const cameraProgress = interpolate(
    frame,
    [confirmShortFrame - 5, confirmShortFrame + 20],
    [0, 1],
    { extrapolateLeft: "clamp", extrapolateRight: "clamp" }
  );
  // No zoom-out in FastPipeline: ConfirmWidget is present in TUI state 7
  // (abs 490–569) all the way to the last frame of FastPipeline (abs 569).
  // There is no post-accept window to zoom back into — the hard cut to
  // SceneComplete resets the camera naturally.
  const cameraScale =
    frame < confirmShortFrame
      ? 1.0
      : interpolate(cameraProgress, [0, 1], [1.0, 1.15]);

  const cameraY =
    frame < confirmShortFrame
      ? 0
      : interpolate(cameraProgress, [0, 1], [0, -40]);

  const completedCount = state.steps.filter(
    (s) => s.status === "success" || s.status === "warning"
  ).length;

  return (
    <div
      style={{
        width: "100%",
        height: "100%",
        background: COLORS.background,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      <div
        style={{
          transform: `scale(${cameraScale}) translateY(${cameraY}px)`,
          transition: "none",
        }}
      >
        <TerminalWindow
          width={1200}
          height={640}
          glowIntensity={(completedCount / 5) * 0.4}
        >
          <TuiLayout
            steps={state.steps}
            activeStepIndex={state.activeStepIndex}
            outputLines={state.outputLines}
            confirmWidget={state.confirmWidget}
            currentFrame={absoluteFrame}
          />
        </TerminalWindow>
      </div>
    </div>
  );
};

// Short clip: 1080 frames (18s at 60fps)
// Hook: 60f, FastPipeline: 660f (full pipeline compressed), Complete: 120f, CTA: 240f
//
// The outer container is always 1920×1080. On non-16:9 canvases (e.g. 1:1) this
// intentionally overflows so the terminal shows the left-hand side prominently.
// The CTA uses a canvas-sized absolute overlay so its layout centres correctly
// regardless of aspect ratio.
// In square (1:1) mode the terminal is shifted 15% of the canvas width to the left
// so more of the left panel (logo + steps) is visible.
const SQUARE_TERM_OFFSET = -162; // 15% of 1080px

export const ShortClip: React.FC = () => {
  const { width, height } = useVideoConfig();
  const isSquare = width === height;
  const termShift = isSquare ? SQUARE_TERM_OFFSET : 0;
  const shiftStyle = isSquare
    ? { transform: `translateX(${SQUARE_TERM_OFFSET}px)` }
    : {};
  return (
    <div style={{ position: "relative", width: 1920, height: 1080 }}>
      <Sequence from={0} durationInFrames={60}>
        <div style={{ width: "100%", height: "100%", ...shiftStyle }}>
          <InstantHook />
        </div>
      </Sequence>
      <Sequence from={60} durationInFrames={660}>
        <div style={{ width: "100%", height: "100%", ...shiftStyle }}>
          <FastPipeline />
        </div>
      </Sequence>
      <Sequence from={720} durationInFrames={120}>
        <div style={{ width: "100%", height: "100%", ...shiftStyle }}>
          <SceneComplete />
        </div>
      </Sequence>
      {/* CTA: absolute overlay sized to the actual canvas so square layout centres correctly */}
      <Sequence from={840} durationInFrames={240}>
        <div style={{ position: "absolute", left: 0, top: 0, width, height }}>
          <SceneCta
            totalDuration={240}
            layout={isSquare ? "square" : "wide"}
            terminalOffsetX={termShift}
          />
        </div>
      </Sequence>
      <FilmGrain width={1920} height={1080} opacity={0.035} />
      <Vignette intensity={0.55} />
    </div>
  );
};
