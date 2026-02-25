import React from "react";
import { useCurrentFrame } from "remotion";
import { TerminalWindow } from "../components/Terminal/TerminalWindow";
import { TuiLayout } from "../components/Terminal/TuiLayout";
import { getStateAtFrame } from "../data/tui-states";

export const TuiPreview: React.FC = () => {
  const frame = useCurrentFrame();
  // Preview starts at the "sync complete" state (frame 1320+)
  const state = getStateAtFrame(1320 + frame);

  return (
    <div
      style={{
        width: "100%",
        height: "100%",
        background: "#0a0c0b",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      <TerminalWindow width={1200} height={640} glowIntensity={0.3}>
        <TuiLayout
          steps={state.steps}
          activeStepIndex={state.activeStepIndex}
          outputLines={state.outputLines}
          confirmWidget={state.confirmWidget}
        />
      </TerminalWindow>
    </div>
  );
};
