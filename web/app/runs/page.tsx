import { Upcoming } from '@/components/Upcoming';

export default function RunsPage() {
  return (
    <Upcoming title="Runs" subtitle="Execution history and live results" milestone={3}>
      <ul style={{ margin: 0, paddingLeft: 18 }}>
        <li>Run history with state, duration, and pass/fail summary</li>
        <li>A live view per run: frame size, iteration, trial countdown, and search window</li>
        <li>Charts fed from the statistics WebSocket at 1 Hz</li>
        <li>Print-styled reports carrying DUT metadata and appliance identity</li>
      </ul>
    </Upcoming>
  );
}
