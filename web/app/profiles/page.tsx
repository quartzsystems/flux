import { Upcoming } from '@/components/Upcoming';

export default function ProfilesPage() {
  return (
    <Upcoming title="Load profiles" subtitle="Stateful L4-7 traffic" milestone={4}>
      <ul style={{ margin: 0, paddingLeft: 18 }}>
        <li>Client and server address pools</li>
        <li>Application behaviour: HTTP GET, replayed pcap, or raw payload</li>
        <li>Target connections per second, concurrency ceiling, and ramp shape</li>
      </ul>
    </Upcoming>
  );
}
