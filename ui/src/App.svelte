<script lang="ts">
  // Placeholder page (plan M0). The real settings — binding, device,
  // release delay, swallow, sounds, autostart, on-exit — arrive in M4.
  const state = ["Disabled", "Muted", "Talking"];
</script>

<main>
  <h1>Push-to-Talk</h1>
  <p class="placeholder">
    Scaffold build. Settings arrive in milestone&nbsp;M4.
  </p>
  <ul>
    {#each state as s}
      <li><span class="dot"></span>{s}</li>
    {/each}
  </ul>
</main>

<style>
  :global(body) {
    margin: 0;
    background: #14161a;
    color: #e8eaed;
    font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
  }
  main {
    padding: 2rem;
  }
  h1 {
    font-size: 1.35rem;
    margin: 0 0 0.5rem;
  }
  .placeholder {
    color: #9aa0a6;
  }
  ul {
    list-style: none;
    padding: 0;
    display: flex;
    gap: 1.25rem;
    margin-top: 1.5rem;
  }
  li {
    display: flex;
    align-items: center;
    gap: 0.45rem;
  }
  .dot {
    width: 0.7rem;
    height: 0.7rem;
    border-radius: 50%;
    background: #5f6368;
    display: inline-block;
  }
  li:nth-child(2) .dot {
    background: #f28b82;
  }
  li:nth-child(3) .dot {
    background: #81c995;
  }
</style>
