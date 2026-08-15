import {
  Badge,
  Box,
  Button,
  Heading,
  Row,
  Stack,
  Text,
  createWorker,
} from "@codypendent/ui";

export interface AgentCardProps {
  agent: { id: string; name: string; status: "idle" | "running" | "failed" };
}

export function AgentCard({ agent }: AgentCardProps) {
  const tone =
    agent.status === "running"
      ? "positive"
      : agent.status === "failed"
      ? "critical"
      : "muted";

  return (
    <Box id={`agent:${agent.id}`} title={agent.name} border="rounded">
      <Row gap={1}>
        <Text weight="bold">Status:</Text>
        <Badge tone={tone}>{agent.status}</Badge>
      </Row>
      <Button action="open-agent" payload={{ agentId: agent.id }}>
        Open
      </Button>
    </Box>
  );
}

export interface PullRequestPanelProps {
  pr: { number: number; title: string; branch: string; reviewsApproved: boolean };
}

export function PullRequestPanel({ pr }: PullRequestPanelProps) {
  return (
    <Stack gap={1} border="single" title={`PR #${pr.number}`}>
      <Heading value={pr.title} />
      <Row gap={2}>
        <Text>Branch: {pr.branch}</Text>
        <Badge tone={pr.reviewsApproved ? "positive" : "warning"}>
          {pr.reviewsApproved ? "Approved" : "Review Required"}
        </Badge>
      </Row>
      <Row gap={1}>
        <Button action="merge-pr" payload={{ prNumber: pr.number }}>
          Merge
        </Button>
        <Button action="close-pr" payload={{ prNumber: pr.number }}>
          Close
        </Button>
      </Row>
    </Stack>
  );
}

export function MainView() {
  return (
    <Stack gap={2}>
      <AgentCard
        agent={{ id: "agent-1", name: "Code Reviewer", status: "running" }}
      />
      <PullRequestPanel
        pr={{
          number: 142,
          title: "Implement UI Polish Suite",
          branch: "feature/ui-polish",
          reviewsApproved: true,
        }}
      />
    </Stack>
  );
}

export const worker = createWorker({
  render: () => <MainView />,
  actions: {
    "open-agent": async (payload) => {
      console.log("Opening agent:", payload);
    },
    "merge-pr": async (payload) => {
      console.log("Merging PR:", payload);
    },
    "close-pr": async (payload) => {
      console.log("Closing PR:", payload);
    },
  },
});
