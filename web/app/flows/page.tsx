import { Upcoming } from '@/components/Upcoming';

export default function FlowsPage() {
  return (
    <Upcoming title="Flows" subtitle="Traffic definitions" milestone={2}>
      <ul style={{ margin: 0, paddingLeft: 18 }}>
        <li>Ordered header-stack builder: Ethernet, 802.1Q, QinQ, IPv4, IPv6, TCP, UDP, raw hex</li>
        <li>Field modifiers with increment and random modes</li>
        <li>Frame size as fixed, IMIX, or a random range; rate in pps, bps, or percent of line</li>
        <li>A live summary of what the flow resolves to, and a hex preview of the first frame</li>
      </ul>
    </Upcoming>
  );
}
