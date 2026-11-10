import { serveSources } from '../core/sourceWorkerHost';
import { createDecodeIngress } from './decodeIngress';
serveSources(createDecodeIngress);
