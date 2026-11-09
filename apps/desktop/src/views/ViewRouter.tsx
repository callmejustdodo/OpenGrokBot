// Shows the content area for the current view (frontend architecture, "Screen mapping").
import { useAppStore } from "../store/store";
import { useUi } from "../store/ui";
import AskConversation from "./ask-conversation/AskConversation";
import History from "./history/History";
import Inbox from "./inbox/Inbox";
import Member from "./member/Member";
import MyConversation from "./my-conversation/MyConversation";
import Rules from "./rules/Rules";
import Team from "./team/Team";
import Welcome from "./welcome/Welcome";

export default function ViewRouter() {
  const view = useUi((s) => s.view);
  const hasMirror = useAppStore((s) => s.mirror !== null);
  const noTeam = useAppStore((s) => s.mirror !== null && s.mirror.team === null);
  if (noTeam) return <Welcome />;
  switch (view.kind) {
    case "agent":
      return <MyConversation key={view.agentId} agentId={view.agentId} />;
    case "teammate-agent":
    case "person":
    case "new-question":
      return <AskConversation view={view} />;
    case "inbox":
      return <Inbox />;
    case "team":
      return <Team />;
    case "member":
      return <Member memberId={view.memberId} />;
    case "history":
      return <History />;
    case "rules":
      return <Rules />;
    case "home":
      return hasMirror ? <section className="home" /> : null;
  }
}
