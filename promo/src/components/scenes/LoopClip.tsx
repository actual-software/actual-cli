import React from "react";
import { useCurrentFrame, interpolate } from "remotion";
import { TerminalWindow } from "../Terminal/TerminalWindow";
import { TuiLayout } from "../Terminal/TuiLayout";
import { FilmGrain } from "../effects/FilmGrain";
import { Vignette } from "../effects/Vignette";
import { COLORS } from "../../data/brand";
import { getStateAtFrame, FRAMES } from "../../data/tui-states";

// Loop clip: 600 frames (10s at 60fps), seamless pipeline loop for README embedding.
// Maps the full 960-frame pipeline (REVEAL_END → WRITE_END) into 600 frames.
// Fades in over the first 20 frames and fades out over the last 40 for seamless looping.
export const LoopClip: React.FC = () => {
  const frame = useCurrentFrame();

  // Map 0–599 → REVEAL_END → WRITE_END (960 frames compressed to 600)
  const pipelineDuration = FRAMES.WRITE_END - FRAMES.REVEAL_END;
  const absoluteFrame =
    FRAMES.REVEAL_END + Math.floor((frame * pipelineDuration) / 600);
  const state = getStateAtFrame(absoluteFrame);

  const completedCount = state.steps.filter(
    (s) => s.status === "success" || s.status === "warning"
  ).length;

  // Fade: in over first 20 frames, out over last 40 frames (seamless loop)
  const fadeIn = interpolate(frame, [0, 20], [0, 1], {
    extrapolateRight: "clamp",
  });
  const fadeOut = interpolate(frame, [560, 600], [1, 0], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  const opacity = Math.min(fadeIn, fadeOut);

  return (
    <div style={{ position: "relative", width: 1200, height: 680 }}>
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
        <TerminalWindow
          width={1160}
          height={640}
          glowIntensity={(completedCount / 5) * 0.3}
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
      <FilmGrain width={1200} height={680} opacity={0.03} />
      <Vignette intensity={0.4} />
    </div>
  );
};
