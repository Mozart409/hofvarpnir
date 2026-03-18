# TODO

## Known Issues

1. **Scheduler self-tell warning**: At `crates/hof-core/src/actors/scheduler.rs:93:19`, 
   an actor is sending a `tell` request to itself using a bounded mailbox, which may 
   lead to a deadlock. Consider using `.try_send()` instead.
