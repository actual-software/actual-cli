import React from "react";
import { TerminalWindow } from "../Terminal/TerminalWindow";
import { TuiLayout } from "../Terminal/TuiLayout";
import { COLORS } from "../../data/brand";

const ALL_WAITING = [
  { label: "Environment", status: "waiting" as const },
  { label: "Analysis", status: "waiting" as const },
  { label: "Fetch ADRs", status: "waiting" as const },
  { label: "Tailoring", status: "waiting" as const },
  { label: "Write Files", status: "waiting" as const },
  { label: "Summary", status: "waiting" as const },
];

export const SceneTuiReveal: React.FC = () => (
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
    <TerminalWindow width={1200} height={640}>
      <TuiLayout
        steps={ALL_WAITING}
        activeStepIndex={0}
        outputLines={[]}
      />
    </TerminalWindow>
  </div>
);
