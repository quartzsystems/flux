import { Upcoming } from '@/components/Upcoming';

export default function TopologyPage() {
  return (
    <Upcoming title="Topology" subtitle="Ports, device under test, and flows" milestone={2}>
      <ul style={{ margin: 0, paddingLeft: 18 }}>
        <li>Ports arranged left and right with the device under test between them</li>
        <li>Flows drawn as edges, with live rate and loss once a run is in flight</li>
        <li>Editable DUT label and metadata, carried into the run report</li>
      </ul>
    </Upcoming>
  );
}
