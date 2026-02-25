import React from "react";
import { Sequence, useCurrentFrame, interpolate } from "remotion";
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

  const confirmAbsRelFrame = FRAMES.CONFIRM_APPEAR - FRAMES.REVEAL_END; // 840
  const acceptAbsRelFrame = FRAMES.ACCEPT_FRAME - FRAMES.REVEAL_END; // 930
  const confirmShortFrame = Math.floor(
    (confirmAbsRelFrame * 660) / pipelineDuration
  );
  const acceptShortFrame = Math.floor(
    (acceptAbsRelFrame * 660) / pipelineDuration
  );

  const cameraProgress = interpolate(
    frame,
    [confirmShortFrame - 5, confirmShortFrame + 20],
    [0, 1],
    { extrapolateLeft: "clamp", extrapolateRight: "clamp" }
  );
  const cameraReturnProgress = interpolate(
    frame,
    [acceptShortFrame, acceptShortFrame + 20],
    [0, 1],
    { extrapolateLeft: "clamp", extrapolateRight: "clamp" }
  );

  const cameraScale =
    frame < confirmShortFrame
      ? 1.0
      : frame < acceptShortFrame
        ? interpolate(cameraProgress, [0, 1], [1.0, 1.15])
        : interpolate(cameraReturnProgress, [0, 1], [1.15, 1.0]);

  const cameraY =
    frame < confirmShortFrame
      ? 0
      : frame < acceptShortFrame
        ? interpolate(cameraProgress, [0, 1], [0, -40])
        : interpolate(cameraReturnProgress, [0, 1], [-40, 0]);

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
          />
        </TerminalWindow>
      </div>
    </div>
  );
};

// Short clip: 900 frames (15s at 60fps)
// Hook: 60f, FastPipeline: 660f (full pipeline compressed), Complete: 120f, CTA: 60f
export const ShortClip: React.FC = () => (
  <div style={{ position: "relative", width: 1920, height: 1080 }}>
    <Sequence from={0} durationInFrames={60}>
      <InstantHook />
    </Sequence>
    <Sequence from={60} durationInFrames={660}>
      <FastPipeline />
    </Sequence>
    <Sequence from={720} durationInFrames={120}>
      <SceneComplete />
    </Sequence>
    <Sequence from={840} durationInFrames={60}>
      <SceneCta />
    </Sequence>
    <FilmGrain width={1920} height={1080} opacity={0.035} />
    <Vignette intensity={0.55} />
  </div>
);
