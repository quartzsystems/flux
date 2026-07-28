import { Upcoming } from '@/components/Upcoming';

export default function AnalyticsPage() {
  return (
    <Upcoming title="Analytics" subtitle="Historical time series" milestone={4}>
      <ul style={{ margin: 0, paddingLeft: 18 }}>
        <li>Pick a metric, filter by port, stream, or run, choose a time range</li>
        <li>Charts rendered from the local VictoriaMetrics query API</li>
      </ul>
    </Upcoming>
  );
}
