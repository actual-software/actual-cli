import React from "react";
import { useCurrentFrame, interpolate, spring } from "remotion";
import { TerminalWindow } from "../Terminal/TerminalWindow";
import { TuiLayout } from "../Terminal/TuiLayout";
import { COLORS, SPRING_CONFIGS } from "../../data/brand";

const ALL_WAITING = [
  { label: "Environment", status: "waiting" as const },
  { label: "Analysis", status: "waiting" as const },
  { label: "Fetch ADRs", status: "waiting" as const },
  { label: "Tailoring", status: "waiting" as const },
  { label: "Write Files", status: "waiting" as const },
];

export const SceneTuiReveal: React.FC = () => {
  const frame = useCurrentFrame();

  // Subtle zoom-out settle: 1.05 → 1.0
  const settleProgress = spring({
    frame,
    fps: 60,
    config: SPRING_CONFIGS.settle,
    durationInFrames: 60,
  });
  const scale = interpolate(settleProgress, [0, 1], [1.05, 1.0]);

  // Overall fade in
  const opacity = interpolate(frame, [0, 20], [0, 1], {
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
      <div style={{ transform: `scale(${scale})` }}>
        <TerminalWindow width={1200} height={620}>
          <TuiLayout
            steps={ALL_WAITING}
            activeStepIndex={0}
            outputLines={[]}
          />
        </TerminalWindow>
      </div>
    </div>
  );
};
