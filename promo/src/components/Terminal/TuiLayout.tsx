import React from "react";
import { COLORS } from "../../data/brand";
import { LogoPanel } from "./LogoPanel";
import { StepsPanel, StepDef } from "./StepsPanel";
import { OutputPane } from "./OutputPane";

interface OutputLine {
  text: string;
  appearFrame: number;
  color?: string;
}

interface TuiLayoutProps {
  steps: StepDef[];
  activeStepIndex: number;
  outputLines: OutputLine[];
  confirmWidget?: React.ComponentProps<typeof OutputPane>["confirmWidget"];
  glowIntensity?: number;
}

export const TuiLayout: React.FC<TuiLayoutProps> = ({
  steps,
  activeStepIndex,
  outputLines,
  confirmWidget,
}) => {
  return (
    <div
      style={{
        display: "flex",
        height: "100%",
        background: COLORS.surface,
        color: COLORS.textPrimary,
        overflow: "hidden",
      }}
    >
      {/* Left panel: logo + steps, fixed 340px */}
      <div
        style={{
          width: 340,
          flexShrink: 0,
          display: "flex",
          flexDirection: "column",
          borderRight: `1px solid ${COLORS.borderGreen}33`,
        }}
      >
        <LogoPanel />
        <StepsPanel steps={steps} activeStepIndex={activeStepIndex} />
      </div>

      {/* Right panel: output pane */}
      <div style={{ flexGrow: 1, overflow: "hidden" }}>
        <OutputPane
          lines={outputLines}
          confirmWidget={confirmWidget}
        />
      </div>
    </div>
  );
};
