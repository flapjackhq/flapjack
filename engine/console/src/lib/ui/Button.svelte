<script lang="ts">
  let {
    label,
    type = 'button',
    disabled = false,
    onpress,
  }: {
    label: string;
    type?: 'button' | 'submit';
    disabled?: boolean;
    onpress?: () => void;
  } = $props();
</script>

<button {type} {disabled} onclick={onpress}>{label}</button>
