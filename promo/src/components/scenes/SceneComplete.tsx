import React from "react";
import { useCurrentFrame, spring } from "remotion";
import { TerminalWindow } from "../Terminal/TerminalWindow";
import { TuiLayout } from "../Terminal/TuiLayout";
import { COLORS, SPRING_CONFIGS } from "../../data/brand";
import { getStateAtFrame, FRAMES } from "../../data/tui-states";

export const SceneComplete: React.FC = () => {
  const frame = useCurrentFrame();
  const absoluteFrame = FRAMES.WRITE_END + frame;
  const state = getStateAtFrame(absoluteFrame);

  // Breathe out: scale 1.0 → 0.98 → 1.0 (subtle)
  const breathe = Math.sin((frame / 300) * Math.PI); // 0 → 1 → 0 over 300 frames
  const cameraScale = 1.0 - breathe * 0.02;

  // Glow: peaks at frame 60, then settles
  const glowPeak = spring({
    frame,
    fps: 60,
    config: SPRING_CONFIGS.glowBurst,
    durationInFrames: 60,
  });
  const glowIntensity = Math.min(glowPeak, 1.3) * 0.8; // cap at 1.04 → * 0.8 = 0.83

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
      <div style={{ transform: `scale(${cameraScale})` }}>
        <TerminalWindow width={1200} height={640} glowIntensity={glowIntensity}>
          <TuiLayout
            steps={state.steps}
            activeStepIndex={state.activeStepIndex}
            outputLines={state.outputLines}
          />
        </TerminalWindow>
      </div>
    </div>
  );
};
