import React from "react";
import { useCurrentFrame } from "remotion";
import { TerminalWindow } from "../components/Terminal/TerminalWindow";
import { HeaderBar } from "../components/Terminal/HeaderBar";
import { TuiLayout } from "../components/Terminal/TuiLayout";
import { getStateAtFrame, FRAMES } from "../data/tui-states";

export const PipelinePreview: React.FC = () => {
  const frame = useCurrentFrame();
  // Map frame 0 → FRAMES.REVEAL_END so pipeline starts immediately
  const stateFrame = FRAMES.REVEAL_END + frame;
  const state = getStateAtFrame(stateFrame);

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
      <TerminalWindow width={1200} height={620} glowIntensity={0}>
        <HeaderBar />
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
