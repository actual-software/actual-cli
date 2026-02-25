import React from "react";
import { TerminalWindow } from "../components/Terminal/TerminalWindow";
import { HeaderBar } from "../components/Terminal/HeaderBar";
import { TuiLayout } from "../components/Terminal/TuiLayout";
import { StepDef } from "../components/Terminal/StepsPanel";

const COMPLETED_STEPS: StepDef[] = [
  {
    label: "Environment",
    status: "warning",
    duration: "0.1s",
    completionFrame: 0,
  },
  {
    label: "Analysis",
    status: "success",
    duration: "1.2s",
    completionFrame: 0,
  },
  {
    label: "Fetch ADRs",
    status: "success",
    duration: "0.8s",
    completionFrame: 0,
  },
  { label: "Tailoring", status: "skipped" },
  {
    label: "Write Files",
    status: "success",
    duration: "0.3s",
    completionFrame: 0,
    isActive: true,
  },
];

const OUTPUT_LINES = [
  {
    text: "  Confirming file changes...",
    appearFrame: 0,
    color: "#6b7c6e",
  },
  { text: "  CLAUDE.md  (new file) + 1 rule", appearFrame: 0 },
  { text: "  ├─  + [adr-cap-] added", appearFrame: 0, color: "#6b7c6e" },
  {
    text: "  ├─    + ## Capture Test Rule",
    appearFrame: 0,
    color: "#6b7c6e",
  },
  {
    text: "  └─    + - Test captures work",
    appearFrame: 0,
    color: "#6b7c6e",
  },
  { text: "  Writing files...", appearFrame: 0, color: "#6b7c6e" },
  { text: "  1 created · 0 updated · 0 failed", appearFrame: 0 },
  {
    text: "    ✔ CLAUDE.md    created   v1",
    appearFrame: 0,
    color: "#00FB7E",
  },
  { text: "", appearFrame: 0 },
  {
    text: "  Sync complete: 1 created · 0 updated · 0 failed · 0 rejected  [0.2s total]",
    appearFrame: 0,
    color: "#00FB7E",
  },
];

export const TuiPreview: React.FC = () => {
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
      <div style={{ display: "flex", flexDirection: "column", width: 1200 }}>
        <TerminalWindow width={1200} height={620} glowIntensity={0.3}>
          <HeaderBar />
          <TuiLayout
            steps={COMPLETED_STEPS}
            activeStepIndex={4}
            outputLines={OUTPUT_LINES}
          />
        </TerminalWindow>
      </div>
    </div>
  );
};
