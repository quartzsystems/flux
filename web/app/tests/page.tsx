import { Upcoming } from '@/components/Upcoming';

export default function TestsPage() {
  return (
    <Upcoming title="Tests" subtitle="Manual and RFC 2544 suites" milestone={3}>
      <ul style={{ margin: 0, paddingLeft: 18 }}>
        <li>RFC 2544 throughput, latency, frame loss, and back-to-back wizards</li>
        <li>Standard frame sizes pre-filled: 64, 128, 256, 512, 1024, 1280, 1518</li>
        <li>Manual tests that start and stop a set of flows directly</li>
        <li>Port-conflict checking before a run is allowed to start</li>
      </ul>
    </Upcoming>
  );
}
